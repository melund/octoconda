// SPDX-License-Identifier: GPL-3.0-or-later
// © Tobias Hunger <tobias.hunger@gmail.com>

use std::{
    collections::{BTreeMap, HashSet},
    io::Write as _,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::Context as _;
use rattler_conda_types::{Platform, VersionWithSource};

use crate::config_file::Package;

#[derive(PartialEq, Eq)]
pub enum Status {
    Failed,
    GithubFailed,
    Succeeded,
    Skipped,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let output = match self {
            Status::Failed => "❌",
            Status::GithubFailed => "❌",
            Status::Succeeded => "✔ ",
            Status::Skipped => "❓",
        };
        write!(f, "{output}")
    }
}

pub fn generate_build_script(work_dir: &Path) -> anyhow::Result<()> {
    let build_script = work_dir.join("build.sh");
    let mut file =
        std::fs::File::create_new(build_script).context("Failed to create the build script")?;
    let content = include_str!("../scripts/build.sh");
    file.write_all(content.as_bytes())
        .context("Failed to write build script")?;
    Ok(())
}

pub fn generate_env_file(
    work_dir: &Path,
    config: &crate::config_file::Config,
) -> anyhow::Result<()> {
    let env_file = work_dir.join("env.sh");
    let mut file = std::fs::File::create_new(env_file).context("Failed to create the env file")?;
    let content = format!(
        r#"
TARGET_CHANNEL="{}"
"#,
        config.conda.short_channel()?,
    );
    file.write_all(content.as_bytes())
        .context("Failed to write env.sh")?;
    Ok(())
}

pub struct PackagingStatus {
    pub platform: Platform,
    pub status: Status,
    pub message: String,
}

pub struct VersionPackagingStatus {
    pub version: Option<String>,
    pub status: Vec<PackagingStatus>,
}

pub struct PackageResult {
    pub repository: String,
    pub name: String,
    pub versions: Vec<VersionPackagingStatus>,
}

impl PackageResult {
    fn display_name(&self) -> String {
        let repo_part = self
            .repository
            .rsplit('/')
            .next()
            .unwrap_or(&self.repository);
        if self.name.eq_ignore_ascii_case(repo_part) {
            self.repository.clone()
        } else {
            format!("{} ({})", self.repository, self.name)
        }
    }
}

impl PackagingStatus {
    pub fn github_failed(error: &str) -> Vec<Self> {
        vec![Self {
            platform: rattler_conda_types::Platform::Unknown,
            status: Status::GithubFailed,
            message: error.to_string(),
        }]
    }

    pub fn recipe_generation_failed(platform: Platform) -> Self {
        Self {
            platform,
            status: Status::Failed,
            message: "could not generate package recipe".to_string(),
        }
    }

    pub fn invalid_version() -> Self {
        Self {
            platform: Platform::Unknown,
            status: Status::Failed,
            message: "could not parse version number from github release".to_string(),
        }
    }

    pub fn skip_platform(platform: Platform) -> Self {
        Self {
            platform,
            status: Status::Succeeded,
            message: "already in conda".to_string(),
        }
    }

    pub fn missing_platform(platform: Platform) -> Self {
        Self {
            platform,
            status: Status::Skipped,
            message: "platform file not found".to_string(),
        }
    }

    pub fn success(platform: Platform) -> Self {
        Self {
            platform,
            status: Status::Succeeded,
            message: "ok".to_string(),
        }
    }

    pub fn in_conda_not_on_github(platform: Platform) -> Self {
        Self {
            platform,
            status: Status::Skipped,
            message: "in conda, not on github".to_string(),
        }
    }
}

pub fn report_results(
    results: &[PackageResult],
    total_configured: usize,
    unknown_in_conda: &[String],
) -> String {
    let mut output = String::new();

    if let Some(first) = results.first() {
        output.push_str(&format!(
            "Processed {}/{total_configured} repositories: {}\n\n",
            results.len(),
            first.repository,
        ));
    }

    // Sort by display name for the sections
    let mut sorted_indices: Vec<usize> = (0..results.len()).collect();
    sorted_indices.sort_by_key(|&i| results[i].display_name());

    let mut github_errors: Vec<String> = vec![];
    let mut no_recipe: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut not_on_github: Vec<(String, Vec<String>)> = vec![];
    let mut in_conda: Vec<(String, Vec<String>)> = vec![];
    let mut generated: Vec<(String, Vec<String>)> = vec![];

    for &i in &sorted_indices {
        let pkg = &results[i];
        let display = pkg.display_name();

        let gh_err = pkg
            .versions
            .iter()
            .find(|v| v.status.iter().any(|s| s.status == Status::GithubFailed));
        if let Some(v) = gh_err {
            let msg = v
                .status
                .iter()
                .find(|s| s.status == Status::GithubFailed)
                .map(|s| s.message.as_str())
                .unwrap_or("unknown error");
            github_errors.push(format!("{display}: {msg}"));
            continue;
        }

        let mut pkg_in_conda = vec![];
        let mut pkg_generated = vec![];
        let mut pkg_not_on_github: Vec<String> = vec![];

        for v in &pkg.versions {
            let ver = v.version.as_deref().unwrap_or("?");

            let is_conda_only = v
                .status
                .iter()
                .all(|s| s.message == "in conda, not on github");
            if is_conda_only {
                let mut platforms: Vec<String> =
                    v.status.iter().map(|s| s.platform.to_string()).collect();
                platforms.sort();
                platforms.dedup();
                pkg_not_on_github.push(format!("{ver} ({})", platforms.join(", ")));
                continue;
            }

            let missing: Vec<String> = v
                .status
                .iter()
                .filter(|s| s.status == Status::Skipped && s.message != "in conda, not on github")
                .map(|s| s.platform.to_string())
                .collect();
            let failed: Vec<&PackagingStatus> = v
                .status
                .iter()
                .filter(|s| s.status == Status::Failed)
                .collect();
            let has_generated = v
                .status
                .iter()
                .any(|s| s.status == Status::Succeeded && s.message == "ok");
            let has_in_conda = v
                .status
                .iter()
                .any(|s| s.status == Status::Succeeded && s.message == "already in conda");

            if !failed.is_empty() {
                let details: Vec<String> = failed
                    .iter()
                    .map(|s| {
                        if s.platform == Platform::Unknown {
                            s.message.clone()
                        } else {
                            format!("{} ({})", s.message, s.platform)
                        }
                    })
                    .collect();
                no_recipe
                    .entry((display.clone(), details.join(", ")))
                    .or_default()
                    .push(ver.to_string());
            } else if !has_generated && !has_in_conda {
                no_recipe
                    .entry((display.clone(), "no matching binary".to_string()))
                    .or_default()
                    .push(ver.to_string());
            } else {
                let missing_note = if missing.is_empty() {
                    String::new()
                } else {
                    format!(" (no: {})", missing.join(", "))
                };
                let formatted = format!("{ver}{missing_note}");

                if has_generated {
                    pkg_generated.push(formatted);
                } else {
                    pkg_in_conda.push(formatted);
                }
            }
        }

        if !pkg_not_on_github.is_empty() {
            not_on_github.push((display.clone(), pkg_not_on_github));
        }
        if !pkg_in_conda.is_empty() {
            in_conda.push((display.clone(), pkg_in_conda));
        }
        if !pkg_generated.is_empty() {
            generated.push((display, pkg_generated));
        }
    }

    // Build report sections
    if !github_errors.is_empty() {
        output.push_str(&format!("GitHub errors ({}):\n", github_errors.len()));
        for name in &github_errors {
            output.push_str(&format!("  {name}\n"));
        }
        output.push('\n');
    }

    if !no_recipe.is_empty() {
        output.push_str(&format!("No recipe generated ({}):\n", no_recipe.len()));
        for ((name, reason), versions) in &no_recipe {
            output.push_str(&format!("  {name} {}: {reason}\n", versions.join(", ")));
        }
        output.push('\n');
    }

    if !not_on_github.is_empty() {
        output.push_str(&format!(
            "Package versions in conda, not on GitHub ({}):\n",
            not_on_github.len()
        ));
        for (name, versions) in &not_on_github {
            output.push_str(&format!("  {name}: {}\n", versions.join(", ")));
        }
        output.push('\n');
    }

    if !unknown_in_conda.is_empty() {
        output.push_str(&format!(
            "Unknown packages in conda ({}):\n",
            unknown_in_conda.len()
        ));
        for name in unknown_in_conda {
            output.push_str(&format!("  {name}\n"));
        }
        output.push('\n');
    }

    if !in_conda.is_empty() {
        output.push_str(&format!("OK (in conda) ({}):\n", in_conda.len()));
        for (name, versions) in &in_conda {
            output.push_str(&format!("  {name}: {}\n", versions.join(", ")));
        }
        output.push('\n');
    }

    if !generated.is_empty() {
        output.push_str(&format!("OK (generated) ({}):\n", generated.len()));
        for (name, versions) in &generated {
            output.push_str(&format!("  {name}: {}\n", versions.join(", ")));
        }
    }

    output
}

fn match_platform<'a>(
    patterns: &[regex::Regex],
    assets: &'a [octocrab::models::repos::Asset],
) -> Option<&'a octocrab::models::repos::Asset> {
    let asset_names = assets.iter().map(|a| a.name.as_str()).collect::<Vec<_>>();
    match_platform_names(patterns, &asset_names).map(|index| &assets[index])
}

fn match_platform_names<'a>(patterns: &[regex::Regex], assets: &'a [&'a str]) -> Option<usize> {
    for r in patterns {
        for (index, a) in assets.iter().enumerate() {
            if r.is_match(&a.to_ascii_lowercase()) {
                return Some(index);
            }
        }
    }
    None
}

pub fn generate_packaging_data(
    package: &Package,
    repository: &octocrab::models::Repository,
    releases: &[(octocrab::models::repos::Release, (String, u32))],
    repo_packages: &[rattler_conda_types::RepoDataRecord],
    work_dir: &Path,
) -> anyhow::Result<Vec<VersionPackagingStatus>> {
    let mut result = vec![];

    for (r, (version_string, build_number)) in releases {
        let Ok(version) = rattler_conda_types::Version::from_str(version_string) else {
            result.push(VersionPackagingStatus {
                version: Some(version_string.clone()),
                status: vec![PackagingStatus::invalid_version()],
            });
            continue;
        };
        let version = VersionWithSource::new(version, version_string);
        let mut version_result = vec![];

        let mut found_platforms = HashSet::new();

        for (platform, pattern) in &package.platforms {
            if let Some(asset) = match_platform(&pattern[..], &r.assets[..]) {
                found_platforms.insert(platform);

                if repo_packages.iter().any(|r| {
                    r.package_record.subdir == platform.to_string()
                        && r.package_record.name.as_normalized() == package.name
                        && r.package_record.version == version
                }) {
                    version_result.push(PackagingStatus::skip_platform(*platform));
                    continue;
                }

                version_result.push(generate_package(
                    work_dir,
                    package,
                    version_string,
                    *build_number,
                    platform,
                    repository,
                    asset,
                ));
            }
        }

        for platform in package.platforms.keys() {
            if !found_platforms.contains(platform) {
                version_result.push(PackagingStatus::missing_platform(*platform));
            }
        }

        result.push(VersionPackagingStatus {
            version: Some(format!("{version_string}-{build_number}")),
            status: version_result,
        });
    }

    // Find versions in conda that have no corresponding GitHub release
    let github_versions: Vec<VersionWithSource> = releases
        .iter()
        .filter_map(|(_, (vs, _))| {
            rattler_conda_types::Version::from_str(vs)
                .ok()
                .map(|v| VersionWithSource::new(v, vs))
        })
        .collect();

    let mut conda_only: BTreeMap<String, Vec<PackagingStatus>> = BTreeMap::new();
    for record in repo_packages {
        if record.package_record.name.as_normalized() != package.name {
            continue;
        }
        if github_versions
            .iter()
            .any(|gv| record.package_record.version == *gv)
        {
            continue;
        }
        let version_str = record.package_record.version.to_string();
        let platform =
            Platform::from_str(&record.package_record.subdir).unwrap_or(Platform::Unknown);
        conda_only
            .entry(version_str)
            .or_default()
            .push(PackagingStatus::in_conda_not_on_github(platform));
    }

    for (version_str, statuses) in conda_only {
        result.push(VersionPackagingStatus {
            version: Some(version_str),
            status: statuses,
        });
    }

    Ok(result)
}

fn extract_digest(asset: &octocrab::models::repos::Asset) -> Option<(String, String)> {
    asset.digest.as_ref().map(|d| {
        let digest = d.strip_prefix("sha256:").unwrap();
        ("sha256".to_string(), digest.to_string())
    })
}

/// Map deprecated SPDX license identifiers to their current equivalents.
///
/// Based on <https://spdx.org/licenses/> — covers all 32 deprecated identifiers
/// that have a clear single-expression replacement.
///
/// Omitted (no single-ID replacement, need project-specific SPDX expressions):
///   eCos-2.0, Net-SNMP, Nunit, wxWindows
fn fix_spdx_license(spdx_id: &str) -> &str {
    match spdx_id {
        // GPL family
        "GPL-1.0" => "GPL-1.0-only",
        "GPL-1.0+" => "GPL-1.0-or-later",
        "GPL-2.0" => "GPL-2.0-only",
        "GPL-2.0+" => "GPL-2.0-or-later",
        "GPL-3.0" => "GPL-3.0-only",
        "GPL-3.0+" => "GPL-3.0-or-later",
        // AGPL family
        "AGPL-1.0" => "AGPL-1.0-only",
        "AGPL-1.0+" => "AGPL-1.0-or-later",
        "AGPL-3.0" => "AGPL-3.0-only",
        "AGPL-3.0+" => "AGPL-3.0-or-later",
        // LGPL family
        "LGPL-2.0" => "LGPL-2.0-only",
        "LGPL-2.0+" => "LGPL-2.0-or-later",
        "LGPL-2.1" => "LGPL-2.1-only",
        "LGPL-2.1+" => "LGPL-2.1-or-later",
        "LGPL-3.0" => "LGPL-3.0-only",
        "LGPL-3.0+" => "LGPL-3.0-or-later",
        // GFDL family
        "GFDL-1.1" => "GFDL-1.1-only",
        "GFDL-1.2" => "GFDL-1.2-only",
        "GFDL-1.3" => "GFDL-1.3-only",
        // GPL-with-exception → SPDX WITH expressions
        "GPL-2.0-with-autoconf-exception" => "GPL-2.0-only WITH Autoconf-exception-2.0",
        "GPL-2.0-with-bison-exception" => "GPL-2.0-only WITH Bison-exception-2.2",
        "GPL-2.0-with-classpath-exception" => "GPL-2.0-only WITH Classpath-exception-2.0",
        "GPL-2.0-with-font-exception" => "GPL-2.0-only WITH Font-exception-2.0",
        "GPL-2.0-with-GCC-exception" => "GPL-2.0-only WITH GCC-exception-2.0",
        "GPL-3.0-with-autoconf-exception" => "GPL-3.0-only WITH Autoconf-exception-3.0",
        "GPL-3.0-with-GCC-exception" => "GPL-3.0-only WITH GCC-exception-3.1",
        // BSD consolidation
        "BSD-2-Clause-FreeBSD" => "BSD-2-Clause",
        "BSD-2-Clause-NetBSD" => "BSD-2-Clause",
        // Other renames
        "bzip2-1.0.5" => "bzip2-1.0.6",
        "StandardML-NJ" => "SMLNJ",
        l => l,
    }
}

/// Escape a string for use inside a YAML double-quoted value.
fn yaml_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn extract_about(
    package_version: &str,
    repository: &octocrab::models::Repository,
    asset: &octocrab::models::repos::Asset,
) -> String {
    let extra_section = {
        let upstream_digest = extract_digest(asset)
            .map(|(algo, digest)| format!("\n  upstream-{algo}: \"{}\"", yaml_escape(&digest)))
            .unwrap_or_default();
        let upstream_version =
            format!("\n  upstream-version: \"{}\"", yaml_escape(package_version));
        let upstream_repository = repository
            .html_url
            .as_ref()
            .map(|u| u.path()[1..].to_string()) // strip leading `/`
            .map(|u| format!("\n  upstream-repository: \"{}\"", yaml_escape(&u)))
            .unwrap_or_default();
        let download_url = format!(
            "\n  release-download-url: \"{}\"",
            yaml_escape(asset.browser_download_url.as_str())
        );
        format!(
            "extra:\n  upstream-forge: github.com{upstream_digest}{upstream_version}{upstream_repository}{download_url}\n"
        )
    };

    let about_section = {
        let homepage = if let Some(homepage) = &repository.homepage
            && !homepage.is_empty()
        {
            format!("  homepage: \"{}\"\n", yaml_escape(homepage))
        } else {
            String::new()
        };

        let license = if let Some(license) = &repository.license {
            let license_info = fix_spdx_license(&license.spdx_id);
            format!("\n  license: \"{}\"", yaml_escape(license_info))
        } else {
            String::new()
        };
        let summary_text = if let Some(description) = &repository.description {
            description.trim().to_owned()
        } else {
            String::new()
        };
        let summary = if let Some(description) = &repository.description {
            format!("\n  summary: \"{}\"", yaml_escape(description.trim()))
        } else {
            String::new()
        };

        format!(
            r#"
about:
  description: >
    {summary_text}

    ... repackaged from github release.

    No files were modified, so all SHAs should match the github release files.
    Files might have been moved, but no files should have been added or removed
    (except for obvious junk files).

    Check the extra package data for details on where the github release file was
    taken from.
{homepage}{license}{summary}"#,
        )
    };

    format!(
        r#"{extra_section}
{about_section}"#
    )
}

fn generate_rattler_build_recipe(
    work_dir: &Path,
    package_name: &str,
    package_version: &str,
    build_number: u32,
    target_platform: &Platform,
    repository: &octocrab::models::Repository,
    asset: &octocrab::models::repos::Asset,
) -> anyhow::Result<PathBuf> {
    let platform_dir = work_dir.join(format!("{target_platform}",));
    let recipe_dir = platform_dir.join(format!("{package_name}-{package_version}-{build_number}",));
    std::fs::create_dir_all(&recipe_dir).context("Failed to create recipe directory")?;

    let build_script_source = work_dir.join("build.sh");
    let build_script_destination = recipe_dir.join("build.sh");
    std::fs::copy(&build_script_source, &build_script_destination).context(format!(
        "Failed to copy build script from {build_script_source:?} to {build_script_destination:?}"
    ))?;

    let recipe_file = recipe_dir.join("recipe.yaml");
    let mut file = std::fs::File::create_new(&recipe_file).context(format!(
        "Failed to create recipe file \"{}\"",
        recipe_file.display()
    ))?;

    let url = asset.browser_download_url.to_string();
    let digest = extract_digest(asset)
        .map(|(algo, value)| format!("\n  {algo}: {value}"))
        .unwrap_or_default();

    let about = extract_about(package_version, repository, asset);
    let pn = package_name;

    let archive = format!("{pn}-{package_version}-{target_platform}");

    let url = yaml_escape(&url);
    let package_version = yaml_escape(package_version);
    let archive = yaml_escape(&archive);

    let content = format!(
        r#"package:
  name: {pn}
  version: "{package_version}"

source:
  url: "{url}"{digest}
  file_name: "{archive}"

build:
  number: {build_number}
  dynamic_linking:
    binary_relocation: false
  prefix_detection:
    ignore: true

tests:
  - package_contents:
      files:
        not_exists:
          - .*
      bin:
        - "*"

{about}"#,
    );

    file.write_all(content.as_bytes()).context(format!(
        "Failed to populate recipe file \"{}\"",
        recipe_file.display(),
    ))?;

    Ok(recipe_dir)
}

fn generate_package(
    work_dir: &Path,
    package: &Package,
    package_version: &str,
    build_number: u32,
    target_platform: &Platform,
    repository: &octocrab::models::Repository,
    asset: &octocrab::models::repos::Asset,
) -> PackagingStatus {
    match generate_rattler_build_recipe(
        work_dir,
        &package.name,
        package_version,
        build_number,
        target_platform,
        repository,
        asset,
    ) {
        Ok(_) => PackagingStatus::success(*target_platform),
        Err(_e) => PackagingStatus::recipe_generation_failed(*target_platform),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config_file::tests::get_patterns_for;

    fn zoxide_names() -> Vec<&'static str> {
        vec![
            "zoxide-0.9.8-aarch64-apple-darwin.tar.gz",
            "zoxide-0.9.8-aarch64-linux-android.tar.gz",
            "zoxide-0.9.8-aarch64-pc-windows-msvc.zip",
            "zoxide-0.9.8-aarch64-unknown-linux-musl.tar.gz",
            "zoxide-0.9.8-arm-unknown-linux-musleabihf.tar.gz",
            "zoxide-0.9.8-armv7-unknown-linux-musleabihf.tar.gz",
            "zoxide-0.9.8-i686-unknown-linux-musl.tar.gz",
            "zoxide-0.9.8-x86_64-apple-darwin.tar.gz",
            "zoxide-0.9.8-x86_64-pc-windows-msvc.zip",
            "zoxide-0.9.8-x86_64-unknown-linux-musl.tar.gz",
            "Source code",
        ]
    }

    fn stripe_names() -> Vec<&'static str> {
        vec![
            "stripe-linux-checksums.txt",
            "stripe-mac-checksums.txt",
            "stripe-windows-checksums.txt",
            "stripe_1.37.2_linux_amd64.deb",
            "stripe_1.37.2_linux_amd64.rpm",
            "stripe_1.37.2_linux_arm64.deb",
            "stripe_1.37.2_linux_arm64.rpm",
            "stripe_1.37.2_linux_arm64.tar.gz",
            "stripe_1.37.2_linux_x86_64.tar.gz",
            "stripe_1.37.2_mac-os_arm64.tar.gz",
            "stripe_1.37.2_mac-os_x86_64.tar.gz",
            "stripe_1.37.2_windows_i386.zip",
            "stripe_1.37.2_windows_x86_64.zip",
            "Source code",
        ]
    }

    fn atuin_names() -> Vec<&'static str> {
        vec![
            "atuin-aarch64-apple-darwin-update",
            "atuin-aarch64-apple-darwin.tar.gz",
            "atuin-aarch64-apple-darwin.tar.gz.sha256",
            "atuin-aarch64-unknown-linux-gnu-update",
            "atuin-aarch64-unknown-linux-gnu.tar.gz",
            "atuin-aarch64-unknown-linux-gnu.tar.gz.sha256",
            "atuin-aarch64-unknown-linux-musl-update",
            "atuin-aarch64-unknown-linux-musl.tar.gz",
            "atuin-aarch64-unknown-linux-musl.tar.gz.sha256",
            "atuin-installer.sh",
            "atuin-x86_64-apple-darwin-update",
            "atuin-x86_64-apple-darwin.tar.gz",
            "atuin-x86_64-apple-darwin.tar.gz.sha256",
            "atuin-x86_64-unknown-linux-gnu-update",
            "atuin-x86_64-unknown-linux-gnu.tar.gz",
            "atuin-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "atuin-x86_64-unknown-linux-musl-update",
            "atuin-x86_64-unknown-linux-musl.tar.gz",
            "atuin-x86_64-unknown-linux-musl.tar.gz.sha256",
            "dist-manifest.json",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
            "Source code (zip)",
            "Source code (tar.gz)",
        ]
    }

    fn asm_lsp_names() -> Vec<&'static str> {
        vec![
            "asm-lsp-aarch64-apple-darwin.tar.gz",
            "asm-lsp-x86_64-apple-darwin.tar.gz",
            "asm-lsp-x86_64-unknown-linux-gnu.tar.gz",
        ]
    }

    fn cargo_binstall_names() -> Vec<&'static str> {
        vec![
            "cargo-binstall-aarch64-apple-darwin.full.zip",
            "cargo-binstall-aarch64-apple-darwin.full.zip.sig",
            "cargo-binstall-aarch64-apple-darwin.zip",
            "cargo-binstall-aarch64-apple-darwin.zip.sig",
            "cargo-binstall-aarch64-pc-windows-msvc.full.zip",
            "cargo-binstall-aarch64-pc-windows-msvc.full.zip.sig",
            "cargo-binstall-aarch64-pc-windows-msvc.zip",
            "cargo-binstall-aarch64-pc-windows-msvc.zip.sig",
            "cargo-binstall-aarch64-unknown-linux-gnu.full.tgz",
            "cargo-binstall-aarch64-unknown-linux-gnu.full.tgz.sig",
            "cargo-binstall-aarch64-unknown-linux-gnu.tgz",
            "cargo-binstall-aarch64-unknown-linux-gnu.tgz.sig",
            "cargo-binstall-aarch64-unknown-linux-musl.full.tgz",
            "cargo-binstall-aarch64-unknown-linux-musl.full.tgz.sig",
            "cargo-binstall-aarch64-unknown-linux-musl.tgz",
            "cargo-binstall-aarch64-unknown-linux-musl.tgz.sig",
            "cargo-binstall-armv7-unknown-linux-gnueabihf.full.tgz",
            "cargo-binstall-armv7-unknown-linux-gnueabihf.full.tgz.sig",
            "cargo-binstall-armv7-unknown-linux-gnueabihf.tgz",
            "cargo-binstall-armv7-unknown-linux-gnueabihf.tgz.sig",
            "cargo-binstall-armv7-unknown-linux-musleabihf.full.tgz",
            "cargo-binstall-armv7-unknown-linux-musleabihf.full.tgz.sig",
            "cargo-binstall-armv7-unknown-linux-musleabihf.tgz",
            "cargo-binstall-armv7-unknown-linux-musleabihf.tgz.sig",
            "cargo-binstall-universal-apple-darwin.full.zip",
            "cargo-binstall-universal-apple-darwin.full.zip.sig",
            "cargo-binstall-universal-apple-darwin.zip",
            "cargo-binstall-universal-apple-darwin.zip.sig",
            "cargo-binstall-x86_64-apple-darwin.full.zip",
            "cargo-binstall-x86_64-apple-darwin.full.zip.sig",
            "cargo-binstall-x86_64-apple-darwin.zip",
            "cargo-binstall-x86_64-apple-darwin.zip.sig",
            "cargo-binstall-x86_64-pc-windows-msvc.full.zip",
            "cargo-binstall-x86_64-pc-windows-msvc.full.zip.sig",
            "cargo-binstall-x86_64-pc-windows-msvc.zip",
            "cargo-binstall-x86_64-pc-windows-msvc.zip.sig",
            "cargo-binstall-x86_64-unknown-linux-gnu.full.tgz",
            "cargo-binstall-x86_64-unknown-linux-gnu.full.tgz.sig",
            "cargo-binstall-x86_64-unknown-linux-gnu.tgz",
            "cargo-binstall-x86_64-unknown-linux-gnu.tgz.sig",
            "cargo-binstall-x86_64-unknown-linux-musl.full.tgz",
            "cargo-binstall-x86_64-unknown-linux-musl.full.tgz.sig",
            "cargo-binstall-x86_64-unknown-linux-musl.tgz",
            "cargo-binstall-x86_64-unknown-linux-musl.tgz.sig",
            "minisign.pub",
        ]
    }

    fn bottom_names() -> Vec<&'static str> {
        vec![
            "bottom-0.11.4-1.x86_64.rpm",
            "bottom-musl-0.11.4-1.x86_64.rpm",
            "bottom-musl_0.11.4-1_amd64.deb",
            "bottom-musl_0.11.4-1_arm64.deb",
            "bottom-musl_0.11.4-1_armhf.deb",
            "bottom.desktop",
            "bottom_0.11.4-1_amd64.deb",
            "bottom_0.11.4-1_arm64.deb",
            "bottom_0.11.4-1_armhf.deb",
            "bottom_aarch64-apple-darwin.tar.gz",
            "bottom_aarch64-pc-windows-msvc.tar.gz",
            "bottom_aarch64-unknown-linux-gnu.tar.gz",
            "bottom_aarch64-unknown-linux-musl.tar.gz",
            "bottom_aarch64_installer.msi",
            "bottom_armv7-unknown-linux-gnueabihf.tar.gz",
            "bottom_armv7-unknown-linux-musleabihf.tar.gz",
            "bottom_i686-pc-windows-msvc.zip",
            "bottom_i686-unknown-linux-gnu.tar.gz",
            "bottom_i686-unknown-linux-musl.tar.gz",
            "bottom_powerpc64le-unknown-linux-gnu.tar.gz",
            "bottom_riscv64gc-unknown-linux-gnu.tar.gz",
            "bottom_x86_64-apple-darwin.tar.gz",
            "bottom_x86_64-pc-windows-gnu.zip",
            "bottom_x86_64-pc-windows-msvc.zip",
            "bottom_x86_64-unknown-freebsd-13.5.tar.gz",
            "bottom_x86_64-unknown-freebsd-14.3.tar.gz",
            "bottom_x86_64-unknown-freebsd-15.0.tar.gz",
            "bottom_x86_64-unknown-linux-gnu-2-17.tar.gz",
            "bottom_x86_64-unknown-linux-gnu.tar.gz",
            "bottom_x86_64-unknown-linux-musl.tar.gz",
            "bottom_x86_64_installer.msi",
            "choco.zip",
            "completion.tar.gz",
            "manpage.tar.gz",
        ]
    }

    fn jjui_names() -> Vec<&'static str> {
        vec![
            "jjui-0.9.6-darwin-amd64.zip",
            "jjui-0.9.6-darwin-arm64.zip",
            "jjui-0.9.6-linux-amd64.zip",
            "jjui-0.9.6-linux-arm64.zip",
            "jjui-0.9.6-windows-amd64.zip",
            "jjui-0.9.6-windows-arm64.zip",
        ]
    }

    fn bazelisk_names() -> Vec<&'static str> {
        vec![
            "bazelisk-amd64.deb",
            "bazelisk-arm64.deb",
            "bazelisk-darwin",
            "bazelisk-darwin-amd64",
            "bazelisk-darwin-arm64",
            "bazelisk-linux-amd64",
            "bazelisk-linux-arm64",
            "bazelisk-windows-amd64.exe",
            "bazelisk-windows-arm64.exe",
        ]
    }

    fn caligula_names() -> Vec<&'static str> {
        vec![
            "caligula-aarch64-darwin",
            "caligula-aarch64-linux",
            "caligula-x86_64-darwin",
            "caligula-x86_64-linux",
        ]
    }

    fn neovim_names() -> Vec<&'static str> {
        vec![
            "nvim-linux-arm64.appimage",
            "nvim-linux-arm64.appimage.zsync",
            "nvim-linux-arm64.tar.gz",
            "nvim-linux-x86_64.appimage",
            "nvim-linux-x86_64.appimage.zsync",
            "nvim-linux-x86_64.tar.gz",
            "nvim-macos-arm64.tar.gz",
            "nvim-macos-x86_64.tar.gz",
            "nvim-win-arm64.msi",
            "nvim-win-arm64.zip",
            "nvim-win64.msi",
            "nvim-win64.zip",
        ]
    }

    fn neovim_names_old() -> Vec<&'static str> {
        vec![
            "nvim-linux64.tar.gz",
            "nvim-macos.tar.gz",
            "nvim-win32.zip",
            "nvim-win64.zip",
            "nvim.appimage",
            "nvim.appimage.zsync",
        ]
    }

    fn shellcheck_names() -> Vec<&'static str> {
        vec![
            "shellcheck-v0.11.0.darwin.aarch64.tar.xz",
            "shellcheck-v0.11.0.darwin.x86_64.tar.xz",
            "shellcheck-v0.11.0.linux.aarch64.tar.xz",
            "shellcheck-v0.11.0.linux.armv6hf.tar.xz",
            "shellcheck-v0.11.0.linux.riscv64.tar.xz",
            "shellcheck-v0.11.0.linux.x86_64.tar.xz",
            "shellcheck-v0.11.0.zip",
        ]
    }

    fn glsl_analyzer_names() -> Vec<&'static str> {
        vec![
            "aarch64-linux-musl.zip",
            "aarch64-macos.zip",
            "aarch64-windows.zip",
            "x86_64-linux-musl.zip",
            "x86_64-macos.zip",
            "x86_64-windows.zip",
        ]
    }

    fn lazygit_names() -> Vec<&'static str> {
        vec![
            "lazygit_0.52.0_Darwin_arm64.tar.gz",
            "lazygit_0.52.0_Darwin_x86_64.tar.gz",
            "lazygit_0.52.0_freebsd_32-bit.tar.gz",
            "lazygit_0.52.0_freebsd_arm64.tar.gz",
            "lazygit_0.52.0_freebsd_armv6.tar.gz",
            "lazygit_0.52.0_freebsd_x86_64.tar.gz",
            "lazygit_0.52.0_Linux_32-bit.tar.gz",
            "lazygit_0.52.0_Linux_arm64.tar.gz",
            "lazygit_0.52.0_Linux_armv6.tar.gz",
            "lazygit_0.52.0_Linux_x86_64.tar.gz",
            "lazygit_0.52.0_Windows_32-bit.zip",
            "lazygit_0.52.0_Windows_arm64.zip",
            "lazygit_0.52.0_Windows_armv6.zip",
            "lazygit_0.52.0_Windows_x86_64.zip",
        ]
    }

    #[track_caller]
    fn assert_platform<'a>(
        patterns: &[regex::Regex],
        assets: &'a [&'a str],
        expected: Option<usize>,
    ) {
        let result = match_platform_names(patterns, assets);

        if let Some(index) = &result {
            eprintln!("    Matched: \"{}\" (index: {index})", assets[*index]);
        } else {
            eprintln!("    No match found");
        }

        if let Some(index) = &expected {
            eprintln!("    Expected: \"{}\"", assets[*index]);
        } else {
            eprintln!("    No match expected");
        }

        assert_eq!(result, expected);
    }

    fn platform_match_test(platforms: &[(Platform, usize)], names: &[&str], release_prefix: &str) {
        let mut platform_patterns = get_patterns_for(release_prefix);

        for (platform, expected) in platforms {
            eprintln!("Testing for platform {platform} (expected index: {expected})");
            assert_platform(
                &platform_patterns.remove(platform).unwrap(),
                names,
                Some(*expected),
            );
        }

        for (platform, patterns) in platform_patterns {
            eprintln!("Testing for platform {platform} (defaulted to None)");
            assert_platform(&patterns, names, None);
        }
    }

    #[test]
    fn test_zoxide_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 6),
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 8),
                (Platform::WinArm64, 2),
            ],
            &zoxide_names(),
            "zoxide",
        );
    }

    #[test]
    fn test_stripe_names() {
        platform_match_test(
            &[
                (Platform::LinuxAarch64, 7),
                (Platform::Linux64, 8),
                (Platform::OsxArm64, 9),
                (Platform::Osx64, 10),
                (Platform::Win32, 11),
                (Platform::Win64, 12),
            ],
            &stripe_names(),
            "stripe",
        );
    }

    #[test]
    fn test_atuin_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 17),
                (Platform::LinuxAarch64, 7),
                (Platform::Osx64, 11),
                (Platform::OsxArm64, 1),
            ],
            &atuin_names(),
            "atuin",
        );
    }

    #[test]
    fn test_asm_lsp_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
            ],
            &asm_lsp_names(),
            "asm-lsp",
        );
    }

    #[test]
    fn test_cargo_binstall_names() {
        platform_match_test(
            &[
                (Platform::LinuxAarch64, 14),
                (Platform::Linux64, 42),
                (Platform::Osx64, 30),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 34),
                (Platform::WinArm64, 6),
            ],
            &cargo_binstall_names(),
            "cargo-binstall",
        );
    }

    #[test]
    fn test_bottom_names() {
        platform_match_test(
            &[
                (Platform::LinuxAarch64, 12),
                (Platform::Linux32, 18),
                (Platform::Linux64, 29),
                (Platform::Osx64, 21),
                (Platform::OsxArm64, 9),
                (Platform::Win32, 16),
                (Platform::Win64, 23),
            ],
            &bottom_names(),
            "bottom",
        );
    }

    #[test]
    fn test_jjui_names() {
        platform_match_test(
            &[
                (Platform::LinuxAarch64, 3),
                (Platform::Linux64, 2),
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 4),
                (Platform::WinArm64, 5),
            ],
            &jjui_names(),
            "jjui",
        );
    }

    #[test]
    fn test_bazelisk_names() {
        platform_match_test(
            &[
                (Platform::LinuxAarch64, 6),
                (Platform::Linux64, 5),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 7),
                (Platform::WinArm64, 8),
            ],
            &bazelisk_names(),
            "bazelisk",
        );
    }

    #[test]
    fn test_caligula_names() {
        platform_match_test(
            &[
                (Platform::LinuxAarch64, 1),
                (Platform::Linux64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 0),
            ],
            &caligula_names(),
            "caligula",
        );
    }

    #[test]
    fn test_neovim_names() {
        platform_match_test(
            &[
                (Platform::LinuxAarch64, 2),
                (Platform::Linux64, 5),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 6),
                (Platform::Win64, 11),
                (Platform::WinArm64, 9),
            ],
            &neovim_names(),
            "nvim",
        );
    }

    #[test]
    fn test_neovim_names_old() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::Win32, 2),
                (Platform::Win64, 3),
            ],
            &neovim_names_old(),
            "nvim",
        );
    }

    #[test]
    fn test_shellcheck_names() {
        platform_match_test(
            &[
                (Platform::LinuxAarch64, 2),
                (Platform::Linux64, 5),
                (Platform::OsxArm64, 0),
                (Platform::Osx64, 1),
            ],
            &shellcheck_names(),
            "shellcheck",
        );
    }

    #[test]
    fn test_glsl_analyzer_names() {
        platform_match_test(
            &[
                (Platform::LinuxAarch64, 0),
                (Platform::Linux64, 3),
                (Platform::OsxArm64, 1),
                (Platform::Osx64, 4),
                (Platform::WinArm64, 2),
                (Platform::Win64, 5),
            ],
            &glsl_analyzer_names(),
            "",
        );
    }

    #[test]
    fn test_lazygit_names() {
        platform_match_test(
            &[
                (Platform::LinuxAarch64, 7),
                (Platform::Linux64, 9),
                (Platform::OsxArm64, 0),
                (Platform::Osx64, 1),
                (Platform::WinArm64, 11),
                (Platform::Win64, 13),
                (Platform::Win32, 10),
            ],
            &lazygit_names(),
            "lazygit",
        );
    }

    fn oxfmt_names() -> Vec<&'static str> {
        vec![
            "oxfmt-darwin-arm64",
            "oxfmt-darwin-arm64.tar.gz",
            "oxfmt-darwin-x64",
            "oxfmt-darwin-x64.tar.gz",
            "oxfmt-linux-arm64-gnu",
            "oxfmt-linux-arm64-gnu.tar.gz",
            "oxfmt-linux-arm64-musl",
            "oxfmt-linux-arm64-musl.tar.gz",
            "oxfmt-linux-x64-gnu",
            "oxfmt-linux-x64-gnu.tar.gz",
            "oxfmt-linux-x64-musl",
            "oxfmt-linux-x64-musl.tar.gz",
            "oxfmt-win32-arm64.exe",
            "oxfmt-win32-arm64.zip",
            "oxfmt-win32-x64.exe",
            "oxfmt-win32-x64.zip",
        ]
    }

    #[test]
    fn test_oxfmt_names() {
        platform_match_test(
            &[
                (Platform::LinuxAarch64, 4),
                (Platform::Linux64, 8),
                (Platform::OsxArm64, 0),
                (Platform::Osx64, 2),
                (Platform::Win64, 14),
                (Platform::WinArm64, 12),
            ],
            &oxfmt_names(),
            "oxfmt",
        );
    }

    fn hcloud_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "checksums.txt.sig",
            "hcloud-darwin-amd64.tar.gz",
            "hcloud-darwin-arm64.tar.gz",
            "hcloud-freebsd-386.tar.gz",
            "hcloud-freebsd-amd64.tar.gz",
            "hcloud-freebsd-arm64.tar.gz",
            "hcloud-freebsd-armv6.tar.gz",
            "hcloud-freebsd-armv7.tar.gz",
            "hcloud-linux-386.tar.gz",
            "hcloud-linux-amd64.tar.gz",
            "hcloud-linux-arm64.tar.gz",
            "hcloud-linux-armv6.tar.gz",
            "hcloud-linux-armv7.tar.gz",
            "hcloud-windows-386.zip",
            "hcloud-windows-amd64.zip",
            "hcloud-windows-arm64.zip",
        ]
    }

    #[test]
    fn test_hcloud_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 11),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 15),
                (Platform::WinArm64, 16),
            ],
            &hcloud_names(),
            "",
        );
    }

    #[test]
    fn test_yaml_escape_plain_string() {
        assert_eq!(yaml_escape("hello world"), "hello world");
    }

    #[test]
    fn test_yaml_escape_double_quotes() {
        assert_eq!(yaml_escape(r#"say "hello""#), r#"say \"hello\""#);
    }

    #[test]
    fn test_yaml_escape_backslash() {
        assert_eq!(yaml_escape(r"foo\bar"), r"foo\\bar");
    }

    #[test]
    fn test_yaml_escape_newline() {
        assert_eq!(yaml_escape("line1\nline2"), r"line1\nline2");
    }

    #[test]
    fn test_yaml_escape_carriage_return() {
        assert_eq!(yaml_escape("line1\r\nline2"), r"line1\r\nline2");
    }

    #[test]
    fn test_yaml_escape_tab() {
        assert_eq!(yaml_escape("col1\tcol2"), r"col1\tcol2");
    }

    #[test]
    fn test_yaml_escape_combined() {
        assert_eq!(
            yaml_escape("path\\to\\\"file\"\tnotes\nmore"),
            r#"path\\to\\\"file\"\tnotes\nmore"#,
        );
    }

    #[test]
    fn test_fix_spdx_license_deprecated_gpl() {
        assert_eq!(fix_spdx_license("GPL-1.0"), "GPL-1.0-only");
        assert_eq!(fix_spdx_license("GPL-1.0+"), "GPL-1.0-or-later");
        assert_eq!(fix_spdx_license("GPL-2.0"), "GPL-2.0-only");
        assert_eq!(fix_spdx_license("GPL-2.0+"), "GPL-2.0-or-later");
        assert_eq!(fix_spdx_license("GPL-3.0"), "GPL-3.0-only");
        assert_eq!(fix_spdx_license("GPL-3.0+"), "GPL-3.0-or-later");
    }

    #[test]
    fn test_fix_spdx_license_deprecated_agpl() {
        assert_eq!(fix_spdx_license("AGPL-1.0"), "AGPL-1.0-only");
        assert_eq!(fix_spdx_license("AGPL-1.0+"), "AGPL-1.0-or-later");
        assert_eq!(fix_spdx_license("AGPL-3.0"), "AGPL-3.0-only");
        assert_eq!(fix_spdx_license("AGPL-3.0+"), "AGPL-3.0-or-later");
    }

    #[test]
    fn test_fix_spdx_license_deprecated_lgpl() {
        assert_eq!(fix_spdx_license("LGPL-2.0"), "LGPL-2.0-only");
        assert_eq!(fix_spdx_license("LGPL-2.0+"), "LGPL-2.0-or-later");
        assert_eq!(fix_spdx_license("LGPL-2.1"), "LGPL-2.1-only");
        assert_eq!(fix_spdx_license("LGPL-2.1+"), "LGPL-2.1-or-later");
        assert_eq!(fix_spdx_license("LGPL-3.0"), "LGPL-3.0-only");
        assert_eq!(fix_spdx_license("LGPL-3.0+"), "LGPL-3.0-or-later");
    }

    #[test]
    fn test_fix_spdx_license_deprecated_gfdl() {
        assert_eq!(fix_spdx_license("GFDL-1.1"), "GFDL-1.1-only");
        assert_eq!(fix_spdx_license("GFDL-1.2"), "GFDL-1.2-only");
        assert_eq!(fix_spdx_license("GFDL-1.3"), "GFDL-1.3-only");
    }

    #[test]
    fn test_fix_spdx_license_gpl_with_exception() {
        assert_eq!(
            fix_spdx_license("GPL-2.0-with-autoconf-exception"),
            "GPL-2.0-only WITH Autoconf-exception-2.0"
        );
        assert_eq!(
            fix_spdx_license("GPL-2.0-with-bison-exception"),
            "GPL-2.0-only WITH Bison-exception-2.2"
        );
        assert_eq!(
            fix_spdx_license("GPL-2.0-with-classpath-exception"),
            "GPL-2.0-only WITH Classpath-exception-2.0"
        );
        assert_eq!(
            fix_spdx_license("GPL-2.0-with-font-exception"),
            "GPL-2.0-only WITH Font-exception-2.0"
        );
        assert_eq!(
            fix_spdx_license("GPL-2.0-with-GCC-exception"),
            "GPL-2.0-only WITH GCC-exception-2.0"
        );
        assert_eq!(
            fix_spdx_license("GPL-3.0-with-autoconf-exception"),
            "GPL-3.0-only WITH Autoconf-exception-3.0"
        );
        assert_eq!(
            fix_spdx_license("GPL-3.0-with-GCC-exception"),
            "GPL-3.0-only WITH GCC-exception-3.1"
        );
    }

    #[test]
    fn test_fix_spdx_license_bsd_consolidation() {
        assert_eq!(fix_spdx_license("BSD-2-Clause-FreeBSD"), "BSD-2-Clause");
        assert_eq!(fix_spdx_license("BSD-2-Clause-NetBSD"), "BSD-2-Clause");
    }

    #[test]
    fn test_fix_spdx_license_other_renames() {
        assert_eq!(fix_spdx_license("bzip2-1.0.5"), "bzip2-1.0.6");
        assert_eq!(fix_spdx_license("StandardML-NJ"), "SMLNJ");
    }

    #[test]
    fn test_fix_spdx_license_passthrough() {
        assert_eq!(fix_spdx_license("MIT"), "MIT");
        assert_eq!(fix_spdx_license("Apache-2.0"), "Apache-2.0");
        assert_eq!(fix_spdx_license("BSD-3-Clause"), "BSD-3-Clause");
        assert_eq!(fix_spdx_license("GPL-2.0-only"), "GPL-2.0-only");
    }
}

#[cfg(test)]
mod aqua_registry_tests {
    use rattler_conda_types::Platform;

    use super::match_platform_names;
    use crate::config_file::tests::get_patterns_for;

    #[track_caller]
    fn assert_platform<'a>(
        patterns: &[regex::Regex],
        assets: &'a [&'a str],
        expected: Option<usize>,
    ) {
        let result = match_platform_names(patterns, assets);

        if let Some(index) = &result {
            eprintln!("    Matched: \"{}\" (index: {index})", assets[*index]);
        } else {
            eprintln!("    No match found");
        }

        if let Some(index) = &expected {
            eprintln!("    Expected: \"{}\"", assets[*index]);
        } else {
            eprintln!("    No match expected");
        }

        assert_eq!(result, expected);
    }

    fn platform_match_test(
        platforms: &[(Platform, usize)],
        names: &[&str],
        release_prefix: &str,
    ) {
        let mut platform_patterns = get_patterns_for(release_prefix);

        for (platform, expected) in platforms {
            eprintln!(
                "Testing for platform {platform} (expected index: {expected})"
            );
            assert_platform(
                &platform_patterns.remove(platform).unwrap(),
                names,
                Some(*expected),
            );
        }
    }
    // === Auto-generated from aqua-registry (470 packages, 385 duplicates removed, 92 skipped) ===

    fn r_01mf02_jaq_jaq_names() -> Vec<&'static str> {
        vec![
            "jaq-aarch64-apple-darwin",
            "jaq-aarch64-unknown-linux-gnu",
            "jaq-arm-unknown-linux-gnueabi",
            "jaq-arm-unknown-linux-musleabihf",
            "jaq-armv7-unknown-linux-gnueabihf",
            "jaq-i686-pc-windows-msvc.exe",
            "jaq-i686-unknown-linux-gnu",
            "jaq-i686-unknown-linux-musl",
            "jaq-x86_64-apple-darwin",
            "jaq-x86_64-pc-windows-msvc.exe",
            "jaq-x86_64-unknown-linux-gnu",
            "jaq-x86_64-unknown-linux-musl",
        ]
    }

    #[test]
    fn test_r_01mf02_jaq_jaq_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 11),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 8),
                (Platform::OsxArm64, 0),
            ],
            &r_01mf02_jaq_jaq_names(),
            "jaq",
        );
    }

    fn r_8051enthusiast_biodiff_biodiff_names() -> Vec<&'static str> {
        vec![
            "biodiff-linux-1.2.1.zip",
            "biodiff-macos-1.2.1.zip",
            "biodiff-windows-1.2.1.zip",
        ]
    }

    #[test]
    fn test_r_8051enthusiast_biodiff_biodiff_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
                (Platform::Win32, 2),
                (Platform::Win64, 2),
                (Platform::WinArm64, 2),
            ],
            &r_8051enthusiast_biodiff_biodiff_names(),
            "biodiff",
        );
    }

    fn r_99designs_aws_vault_aws_vault_names() -> Vec<&'static str> {
        vec![
            "aws-vault-darwin-amd64.dmg",
            "aws-vault-darwin-arm64.dmg",
            "aws-vault-freebsd-amd64",
            "aws-vault-linux-amd64",
            "aws-vault-linux-arm64",
            "aws-vault-linux-arm7",
            "aws-vault-linux-ppc64le",
            "aws-vault-windows-386.exe",
            "aws-vault-windows-arm64.exe",
            "SHA256SUMS",
        ]
    }

    #[test]
    fn test_r_99designs_aws_vault_aws_vault_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 1),
            ],
            &r_99designs_aws_vault_aws_vault_names(),
            "aws-vault",
        );
    }

    fn agwa_git_crypt_git_crypt_names() -> Vec<&'static str> {
        vec![
            "git-crypt-0.8.0-linux-x86_64",
        ]
    }

    #[test]
    fn test_agwa_git_crypt_git_crypt_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
            ],
            &agwa_git_crypt_git_crypt_names(),
            "git-crypt",
        );
    }

    fn adembc_lazyssh_lazyssh_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "lazyssh_Darwin_arm64.tar.gz",
            "lazyssh_Darwin_x86_64.tar.gz",
            "lazyssh_Linux_arm64.tar.gz",
            "lazyssh_Linux_armv6.tar.gz",
            "lazyssh_Linux_i386.tar.gz",
            "lazyssh_Linux_x86_64.tar.gz",
            "lazyssh_Windows_arm64.zip",
            "lazyssh_Windows_armv6.zip",
            "lazyssh_Windows_i386.zip",
            "lazyssh_Windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_adembc_lazyssh_lazyssh_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 10),
                (Platform::WinArm64, 7),
            ],
            &adembc_lazyssh_lazyssh_names(),
            "lazyssh",
        );
    }

    fn automattic_harper_harper_cli_names() -> Vec<&'static str> {
        vec![
            "harper-alpine-arm64-1.9.0.vsix",
            "harper-alpine-x64-1.9.0.vsix",
            "harper-chrome-plugin.zip",
            "harper-cli-aarch64-apple-darwin.tar.gz",
            "harper-cli-aarch64-unknown-linux-gnu.tar.gz",
            "harper-cli-aarch64-unknown-linux-musl.tar.gz",
            "harper-cli-x86_64-apple-darwin.tar.gz",
            "harper-cli-x86_64-pc-windows-msvc.zip",
            "harper-cli-x86_64-unknown-linux-gnu.tar.gz",
            "harper-cli-x86_64-unknown-linux-musl.tar.gz",
            "harper-darwin-arm64-1.9.0.vsix",
            "harper-darwin-x64-1.9.0.vsix",
            "harper-firefox-plugin.zip",
            "harper-linux-arm64-1.9.0.vsix",
            "harper-linux-armhf-1.9.0.vsix",
            "harper-linux-x64-1.9.0.vsix",
            "harper-ls-aarch64-apple-darwin.tar.gz",
            "harper-ls-aarch64-unknown-linux-gnu.tar.gz",
            "harper-ls-aarch64-unknown-linux-musl.tar.gz",
            "harper-ls-x86_64-apple-darwin.tar.gz",
            "harper-ls-x86_64-pc-windows-msvc.zip",
            "harper-ls-x86_64-unknown-linux-gnu.tar.gz",
            "harper-ls-x86_64-unknown-linux-musl.tar.gz",
            "harper-win32-arm64-1.9.0.vsix",
            "harper-win32-x64-1.9.0.vsix",
            "harper.zip",
        ]
    }

    #[test]
    fn test_automattic_harper_harper_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 7),
            ],
            &automattic_harper_harper_cli_names(),
            "harper-cli",
        );
    }

    fn avitaltamir_cyphernetes_cyphernetes_names() -> Vec<&'static str> {
        vec![
            "cyphernetes-darwin-amd64",
            "cyphernetes-darwin-arm64",
            "cyphernetes-linux-amd64",
            "cyphernetes-linux-arm64",
            "cyphernetes-windows-amd64.exe",
            "cyphernetes-windows-arm64.exe",
            "kubectl-cypher-darwin-amd64.tar.gz",
            "kubectl-cypher-darwin-arm64.tar.gz",
            "kubectl-cypher-linux-amd64.tar.gz",
            "kubectl-cypher-linux-arm64.tar.gz",
            "kubectl-cypher-windows-amd64.tar.gz",
            "kubectl-cypher-windows-arm64.tar.gz",
        ]
    }

    #[test]
    fn test_avitaltamir_cyphernetes_cyphernetes_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 1),
            ],
            &avitaltamir_cyphernetes_cyphernetes_names(),
            "cyphernetes",
        );
    }

    fn azure_bicep_bicep_names() -> Vec<&'static str> {
        vec![
            "Azure.Bicep.CommandLine.linux-arm64.0.41.2.nupkg",
            "Azure.Bicep.CommandLine.linux-x64.0.41.2.nupkg",
            "Azure.Bicep.CommandLine.osx-arm64.0.41.2.nupkg",
            "Azure.Bicep.CommandLine.osx-x64.0.41.2.nupkg",
            "Azure.Bicep.CommandLine.win-arm64.0.41.2.nupkg",
            "Azure.Bicep.CommandLine.win-x64.0.41.2.nupkg",
            "Azure.Bicep.Core.0.41.2.nupkg",
            "Azure.Bicep.Core.0.41.2.snupkg",
            "Azure.Bicep.Decompiler.0.41.2.nupkg",
            "Azure.Bicep.Decompiler.0.41.2.snupkg",
            "Azure.Bicep.IO.0.41.2.nupkg",
            "Azure.Bicep.IO.0.41.2.snupkg",
            "Azure.Bicep.Local.Extension.0.41.2.nupkg",
            "Azure.Bicep.Local.Extension.0.41.2.snupkg",
            "Azure.Bicep.Local.Rpc.0.41.2.nupkg",
            "Azure.Bicep.Local.Rpc.0.41.2.snupkg",
            "Azure.Bicep.McpServer.0.41.2.nupkg",
            "Azure.Bicep.McpServer.0.41.2.snupkg",
            "Azure.Bicep.MSBuild.0.41.2.nupkg",
            "Azure.Bicep.MSBuild.0.41.2.snupkg",
            "Azure.Bicep.RegistryModuleTool.0.41.2.nupkg",
            "Azure.Bicep.RegistryModuleTool.0.41.2.snupkg",
            "Azure.Bicep.RpcClient.0.41.2.nupkg",
            "Azure.Bicep.RpcClient.0.41.2.snupkg",
            "bicep-langserver.zip",
            "bicep-linux-arm64",
            "bicep-linux-musl-x64",
            "bicep-linux-x64",
            "bicep-osx-arm64",
            "bicep-osx-x64",
            "bicep-setup-win-x64.exe",
            "bicep-win-arm64.exe",
            "bicep-win-x64.exe",
            "vs-bicep.vsix",
            "vscode-bicep.vsix",
        ]
    }

    #[test]
    fn test_azure_bicep_bicep_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 26),
                (Platform::LinuxAarch64, 25),
                (Platform::Osx64, 29),
                (Platform::OsxArm64, 28),
            ],
            &azure_bicep_bicep_names(),
            "bicep",
        );
    }

    fn beaconbay_ck_ck_names() -> Vec<&'static str> {
        vec![
            "ck-0.7.4-aarch64-apple-darwin.tar.gz",
            "ck-0.7.4-aarch64-apple-darwin.tar.gz.sha256",
            "ck-0.7.4-aarch64-pc-windows-msvc.zip",
            "ck-0.7.4-aarch64-pc-windows-msvc.zip.sha256",
            "ck-0.7.4-x86_64-apple-darwin.tar.gz",
            "ck-0.7.4-x86_64-apple-darwin.tar.gz.sha256",
            "ck-0.7.4-x86_64-pc-windows-msvc.zip",
            "ck-0.7.4-x86_64-pc-windows-msvc.zip.sha256",
            "ck-0.7.4-x86_64-unknown-linux-gnu.tar.gz",
            "ck-0.7.4-x86_64-unknown-linux-gnu.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_beaconbay_ck_ck_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 6),
                (Platform::WinArm64, 2),
            ],
            &beaconbay_ck_ck_names(),
            "ck",
        );
    }

    fn bearer_gon_gon_names() -> Vec<&'static str> {
        vec![
            "gon_macos.zip",
        ]
    }

    #[test]
    fn test_bearer_gon_gon_names() {
        platform_match_test(
            &[
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 0),
            ],
            &bearer_gon_gon_names(),
            "gon",
        );
    }

    fn bishopfox_cloudfox_cloudfox_names() -> Vec<&'static str> {
        vec![
            "cloudfox-linux-386.zip",
            "cloudfox-linux-amd64.zip",
            "cloudfox-linux-arm64.zip",
            "cloudfox-macos-amd64.zip",
            "cloudfox-macos-arm64.zip",
            "cloudfox-windows-amd64.zip",
        ]
    }

    #[test]
    fn test_bishopfox_cloudfox_cloudfox_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 5),
            ],
            &bishopfox_cloudfox_cloudfox_names(),
            "cloudfox",
        );
    }

    fn builditluc_wiki_tui_wiki_tui_names() -> Vec<&'static str> {
        vec![
            "wiki-tui-linux.sha256",
            "wiki-tui-linux.tar.gz",
            "wiki-tui-macos.sha256",
            "wiki-tui-macos.tar.gz",
            "wiki-tui-windows.sha256",
            "wiki-tui-windows.tar.gz",
        ]
    }

    #[test]
    fn test_builditluc_wiki_tui_wiki_tui_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
            ],
            &builditluc_wiki_tui_wiki_tui_names(),
            "wiki-tui",
        );
    }

    fn burntsushi_ripgrep_ripgrep_names() -> Vec<&'static str> {
        vec![
            "ripgrep-15.1.0-aarch64-apple-darwin.tar.gz",
            "ripgrep-15.1.0-aarch64-apple-darwin.tar.gz.sha256",
            "ripgrep-15.1.0-aarch64-pc-windows-msvc.zip",
            "ripgrep-15.1.0-aarch64-pc-windows-msvc.zip.sha256",
            "ripgrep-15.1.0-aarch64-unknown-linux-gnu.tar.gz",
            "ripgrep-15.1.0-aarch64-unknown-linux-gnu.tar.gz.sha256",
            "ripgrep-15.1.0-armv7-unknown-linux-gnueabihf.tar.gz",
            "ripgrep-15.1.0-armv7-unknown-linux-gnueabihf.tar.gz.sha256",
            "ripgrep-15.1.0-armv7-unknown-linux-musleabi.tar.gz",
            "ripgrep-15.1.0-armv7-unknown-linux-musleabi.tar.gz.sha256",
            "ripgrep-15.1.0-armv7-unknown-linux-musleabihf.tar.gz",
            "ripgrep-15.1.0-armv7-unknown-linux-musleabihf.tar.gz.sha256",
            "ripgrep-15.1.0-i686-pc-windows-msvc.zip",
            "ripgrep-15.1.0-i686-pc-windows-msvc.zip.sha256",
            "ripgrep-15.1.0-i686-unknown-linux-gnu.tar.gz",
            "ripgrep-15.1.0-i686-unknown-linux-gnu.tar.gz.sha256",
            "ripgrep-15.1.0-s390x-unknown-linux-gnu.tar.gz",
            "ripgrep-15.1.0-s390x-unknown-linux-gnu.tar.gz.sha256",
            "ripgrep-15.1.0-x86_64-apple-darwin.tar.gz",
            "ripgrep-15.1.0-x86_64-apple-darwin.tar.gz.sha256",
            "ripgrep-15.1.0-x86_64-pc-windows-gnu.zip",
            "ripgrep-15.1.0-x86_64-pc-windows-gnu.zip.sha256",
            "ripgrep-15.1.0-x86_64-pc-windows-msvc.zip",
            "ripgrep-15.1.0-x86_64-pc-windows-msvc.zip.sha256",
            "ripgrep-15.1.0-x86_64-unknown-linux-musl.tar.gz",
            "ripgrep-15.1.0-x86_64-unknown-linux-musl.tar.gz.sha256",
            "ripgrep_15.1.0-1_amd64.deb",
            "ripgrep_15.1.0-1_amd64.deb.sha256",
        ]
    }

    #[test]
    fn test_burntsushi_ripgrep_ripgrep_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 24),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 18),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 22),
                (Platform::WinArm64, 2),
            ],
            &burntsushi_ripgrep_ripgrep_names(),
            "ripgrep",
        );
    }

    fn byron_dua_cli_dua_names() -> Vec<&'static str> {
        vec![
            "dua-v2.34.0-aarch64-unknown-linux-musl.tar.gz",
            "dua-v2.34.0-arm-unknown-linux-gnueabihf.tar.gz",
            "dua-v2.34.0-i686-pc-windows-msvc.zip",
            "dua-v2.34.0-x86_64-apple-darwin.tar.gz",
            "dua-v2.34.0-x86_64-pc-windows-msvc.zip",
            "dua-v2.34.0-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_byron_dua_cli_dua_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 3),
                (Platform::Win64, 4),
            ],
            &byron_dua_cli_dua_names(),
            "dua",
        );
    }

    fn cqlabs_homebrew_dcm_dcm_names() -> Vec<&'static str> {
        vec![
            "dcm-linux-arm-release.zip",
            "dcm-linux-x64-release.zip",
            "dcm-macos-arm-release.zip",
            "dcm-macos-x64-release.zip",
            "dcm-windows-release.zip",
            "dcm_1.35.2-1_amd64.deb",
            "dcm_1.35.2-1_arm64.deb",
        ]
    }

    #[test]
    fn test_cqlabs_homebrew_dcm_dcm_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 2),
                (Platform::Win32, 4),
                (Platform::Win64, 4),
                (Platform::WinArm64, 4),
            ],
            &cqlabs_homebrew_dcm_dcm_names(),
            "dcm",
        );
    }

    fn canop_rhit_rhit_names() -> Vec<&'static str> {
        vec![
            "rhit_2.0.4.zip",
        ]
    }

    #[test]
    fn test_canop_rhit_rhit_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 0),
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 0),
                (Platform::Win32, 0),
                (Platform::Win64, 0),
                (Platform::WinArm64, 0),
            ],
            &canop_rhit_rhit_names(),
            "rhit",
        );
    }

    fn cian911_switchboard_switchboard_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "switchboard_1.0.1_darwin_amd64.tar.gz",
            "switchboard_1.0.1_darwin_arm64.tar.gz",
            "switchboard_1.0.1_linux_386.apk",
            "switchboard_1.0.1_linux_386.deb",
            "switchboard_1.0.1_linux_386.rpm",
            "switchboard_1.0.1_linux_386.tar.gz",
            "switchboard_1.0.1_linux_amd64.apk",
            "switchboard_1.0.1_linux_amd64.deb",
            "switchboard_1.0.1_linux_amd64.rpm",
            "switchboard_1.0.1_linux_amd64.tar.gz",
            "switchboard_1.0.1_linux_arm64.apk",
            "switchboard_1.0.1_linux_arm64.deb",
            "switchboard_1.0.1_linux_arm64.rpm",
            "switchboard_1.0.1_linux_arm64.tar.gz",
            "switchboard_1.0.1_linux_armv5.apk",
            "switchboard_1.0.1_linux_armv5.deb",
            "switchboard_1.0.1_linux_armv5.rpm",
            "switchboard_1.0.1_linux_armv5.tar.gz",
            "switchboard_1.0.1_linux_armv6.apk",
            "switchboard_1.0.1_linux_armv6.deb",
            "switchboard_1.0.1_linux_armv6.rpm",
            "switchboard_1.0.1_linux_armv6.tar.gz",
            "switchboard_1.0.1_linux_armv7.apk",
            "switchboard_1.0.1_linux_armv7.deb",
            "switchboard_1.0.1_linux_armv7.rpm",
            "switchboard_1.0.1_linux_armv7.tar.gz",
            "switchboard_1.0.1_linux_riscv64.apk",
            "switchboard_1.0.1_linux_riscv64.deb",
            "switchboard_1.0.1_linux_riscv64.rpm",
            "switchboard_1.0.1_linux_riscv64.tar.gz",
            "switchboard_1.0.1_windows_386.tar.gz",
            "switchboard_1.0.1_windows_amd64.tar.gz",
            "switchboard_1.0.1_windows_arm64.tar.gz",
            "switchboard_1.0.1_windows_armv5.tar.gz",
            "switchboard_1.0.1_windows_armv6.tar.gz",
            "switchboard_1.0.1_windows_armv7.tar.gz",
        ]
    }

    #[test]
    fn test_cian911_switchboard_switchboard_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 6),
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 14),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win32, 31),
                (Platform::Win64, 32),
                (Platform::WinArm64, 33),
            ],
            &cian911_switchboard_switchboard_names(),
            "switchboard",
        );
    }

    fn clementtsang_bottom_bottom_names() -> Vec<&'static str> {
        vec![
            "bottom-0.12.3-1.x86_64.rpm",
            "bottom-musl-0.12.3-1.x86_64.rpm",
            "bottom-musl_0.12.3-1_amd64.deb",
            "bottom-musl_0.12.3-1_arm64.deb",
            "bottom-musl_0.12.3-1_armhf.deb",
            "bottom.desktop",
            "bottom_0.12.3-1_amd64.deb",
            "bottom_0.12.3-1_arm64.deb",
            "bottom_0.12.3-1_armhf.deb",
            "bottom_aarch64-apple-darwin.tar.gz",
            "bottom_aarch64-linux-android.tar.gz",
            "bottom_aarch64-pc-windows-msvc.tar.gz",
            "bottom_aarch64-unknown-linux-gnu.tar.gz",
            "bottom_aarch64-unknown-linux-musl.tar.gz",
            "bottom_aarch64_installer.msi",
            "bottom_armv7-unknown-linux-gnueabihf.tar.gz",
            "bottom_armv7-unknown-linux-musleabihf.tar.gz",
            "bottom_i686-pc-windows-msvc.zip",
            "bottom_i686-unknown-linux-gnu.tar.gz",
            "bottom_i686-unknown-linux-musl.tar.gz",
            "bottom_loongarch64-unknown-linux-gnu.tar.gz",
            "bottom_powerpc64le-unknown-linux-gnu.tar.gz",
            "bottom_riscv64gc-unknown-linux-gnu.tar.gz",
            "bottom_x86_64-apple-darwin.tar.gz",
            "bottom_x86_64-pc-windows-gnu.zip",
            "bottom_x86_64-pc-windows-msvc.zip",
            "bottom_x86_64-unknown-freebsd-13.5.tar.gz",
            "bottom_x86_64-unknown-freebsd-14.3.tar.gz",
            "bottom_x86_64-unknown-freebsd-15.0.tar.gz",
            "bottom_x86_64-unknown-linux-gnu-2-17.tar.gz",
            "bottom_x86_64-unknown-linux-gnu.tar.gz",
            "bottom_x86_64-unknown-linux-musl.tar.gz",
            "bottom_x86_64_installer.msi",
            "choco.zip",
            "completion.tar.gz",
            "manpage.tar.gz",
        ]
    }

    #[test]
    fn test_clementtsang_bottom_bottom_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 31),
                (Platform::LinuxAarch64, 13),
                (Platform::Osx64, 23),
                (Platform::OsxArm64, 9),
                (Platform::Win64, 25),
            ],
            &clementtsang_bottom_bottom_names(),
            "bottom",
        );
    }

    fn code_hex_gqldoc_gqldoc_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "gqldoc_0.0.6_Linux_arm.tar.gz",
            "gqldoc_0.0.6_Linux_arm64.tar.gz",
            "gqldoc_0.0.6_Linux_i386.tar.gz",
            "gqldoc_0.0.6_Linux_x86_64.tar.gz",
            "gqldoc_0.0.6_macOS_arm64.tar.gz",
            "gqldoc_0.0.6_macOS_x86_64.tar.gz",
            "gqldoc_0.0.6_Windows_arm.zip",
            "gqldoc_0.0.6_Windows_arm64.zip",
            "gqldoc_0.0.6_Windows_i386.zip",
            "gqldoc_0.0.6_Windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_code_hex_gqldoc_gqldoc_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 5),
                (Platform::Win64, 10),
                (Platform::WinArm64, 8),
            ],
            &code_hex_gqldoc_gqldoc_names(),
            "gqldoc",
        );
    }

    fn cyberagent_reminder_lint_reminder_lint_names() -> Vec<&'static str> {
        vec![
            "dist-manifest.json",
            "reminder-lint-aarch64-apple-darwin.tar.xz",
            "reminder-lint-aarch64-apple-darwin.tar.xz.sha256",
            "reminder-lint-installer.sh",
            "reminder-lint-x86_64-apple-darwin.tar.xz",
            "reminder-lint-x86_64-apple-darwin.tar.xz.sha256",
            "reminder-lint-x86_64-pc-windows-msvc.zip",
            "reminder-lint-x86_64-pc-windows-msvc.zip.sha256",
            "reminder-lint-x86_64-unknown-linux-gnu.tar.xz",
            "reminder-lint-x86_64-unknown-linux-gnu.tar.xz.sha256",
            "reminder-lint-x86_64-unknown-linux-musl.tar.xz",
            "reminder-lint-x86_64-unknown-linux-musl.tar.xz.sha256",
            "reminder-lint.rb",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_cyberagent_reminder_lint_reminder_lint_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 10),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 6),
            ],
            &cyberagent_reminder_lint_reminder_lint_names(),
            "reminder-lint",
        );
    }

    fn cyclonedx_cyclonedx_cli_cyclonedx_names() -> Vec<&'static str> {
        vec![
            "cyclonedx-linux-arm",
            "cyclonedx-linux-arm64",
            "cyclonedx-linux-musl-x64",
            "cyclonedx-linux-x64",
            "cyclonedx-osx-arm64",
            "cyclonedx-osx-x64",
            "cyclonedx-win-arm64.exe",
            "cyclonedx-win-x64.exe",
            "cyclonedx-win-x86.exe",
        ]
    }

    #[test]
    fn test_cyclonedx_cyclonedx_cli_cyclonedx_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 4),
            ],
            &cyclonedx_cyclonedx_cli_cyclonedx_names(),
            "cyclonedx",
        );
    }

    fn delineaxpm_dsv_cli_dsv_names() -> Vec<&'static str> {
        vec![
            "checksums-sha256.txt",
            "cli-version.json",
            "dsv-darwin-amd64.sbom.json",
            "dsv-darwin-arm64",
            "dsv-darwin-arm64.sbom.json",
            "dsv-darwin-x64",
            "dsv-linux-386.sbom.json",
            "dsv-linux-amd64.sbom.json",
            "dsv-linux-x64",
            "dsv-linux-x86",
            "dsv-win-x64.exe",
            "dsv-win-x64.zip",
            "dsv-win-x86.exe",
            "dsv-win-x86.zip",
            "dsv.exe-windows-386.sbom.json",
            "dsv.exe-windows-amd64.sbom.json",
        ]
    }

    #[test]
    fn test_delineaxpm_dsv_cli_dsv_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 3),
            ],
            &delineaxpm_dsv_cli_dsv_names(),
            "dsv",
        );
    }

    fn edjopato_mqttui_mqttui_names() -> Vec<&'static str> {
        vec![
            "mqttui-v0.22.1-aarch64-apple-darwin.tar.gz",
            "mqttui-v0.22.1-aarch64-pc-windows-msvc.zip",
            "mqttui-v0.22.1-aarch64-unknown-linux-gnu.deb",
            "mqttui-v0.22.1-aarch64-unknown-linux-gnu.rpm",
            "mqttui-v0.22.1-aarch64-unknown-linux-gnu.tar.gz",
            "mqttui-v0.22.1-arm-unknown-linux-gnueabihf.deb",
            "mqttui-v0.22.1-arm-unknown-linux-gnueabihf.tar.gz",
            "mqttui-v0.22.1-armv7-unknown-linux-gnueabihf.deb",
            "mqttui-v0.22.1-armv7-unknown-linux-gnueabihf.rpm",
            "mqttui-v0.22.1-armv7-unknown-linux-gnueabihf.tar.gz",
            "mqttui-v0.22.1-riscv64gc-unknown-linux-gnu.deb",
            "mqttui-v0.22.1-riscv64gc-unknown-linux-gnu.tar.gz",
            "mqttui-v0.22.1-x86_64-apple-darwin.tar.gz",
            "mqttui-v0.22.1-x86_64-pc-windows-msvc.zip",
            "mqttui-v0.22.1-x86_64-unknown-linux-gnu.deb",
            "mqttui-v0.22.1-x86_64-unknown-linux-gnu.rpm",
            "mqttui-v0.22.1-x86_64-unknown-linux-gnu.tar.gz",
        ]
    }

    #[test]
    fn test_edjopato_mqttui_mqttui_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 16),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 12),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 13),
                (Platform::WinArm64, 1),
            ],
            &edjopato_mqttui_mqttui_names(),
            "mqttui",
        );
    }

    fn edeneast_repo_repo_names() -> Vec<&'static str> {
        vec![
            "repo-x86_64-apple-darwin.tar.gz",
            "repo-x86_64-apple-darwin.tar.gz.sha256",
            "repo-x86_64-pc-windows-msvc.zip",
            "repo-x86_64-pc-windows-msvc.zip.sha256",
            "repo-x86_64-unknown-linux-gnu.tar.gz",
            "repo-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "repo-x86_64-unknown-linux-musl.tar.gz",
            "repo-x86_64-unknown-linux-musl.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_edeneast_repo_repo_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::Osx64, 0),
                (Platform::Win64, 2),
            ],
            &edeneast_repo_repo_names(),
            "repo",
        );
    }

    fn embarkstudios_cargo_deny_cargo_deny_names() -> Vec<&'static str> {
        vec![
            "cargo-deny-0.19.0-aarch64-apple-darwin.tar.gz",
            "cargo-deny-0.19.0-aarch64-apple-darwin.tar.gz.sha256",
            "cargo-deny-0.19.0-aarch64-unknown-linux-musl.tar.gz",
            "cargo-deny-0.19.0-aarch64-unknown-linux-musl.tar.gz.sha256",
            "cargo-deny-0.19.0-x86_64-apple-darwin.tar.gz",
            "cargo-deny-0.19.0-x86_64-apple-darwin.tar.gz.sha256",
            "cargo-deny-0.19.0-x86_64-pc-windows-msvc.tar.gz",
            "cargo-deny-0.19.0-x86_64-pc-windows-msvc.tar.gz.sha256",
            "cargo-deny-0.19.0-x86_64-unknown-linux-musl.tar.gz",
            "cargo-deny-0.19.0-x86_64-unknown-linux-musl.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_embarkstudios_cargo_deny_cargo_deny_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 6),
            ],
            &embarkstudios_cargo_deny_cargo_deny_names(),
            "cargo-deny",
        );
    }

    fn epistates_treemd_treemd_names() -> Vec<&'static str> {
        vec![
            "SHA256SUMS",
            "treemd-aarch64-apple-darwin.sha256",
            "treemd-aarch64-apple-darwin.tar.gz",
            "treemd-aarch64-unknown-linux-gnu.sha256",
            "treemd-aarch64-unknown-linux-gnu.tar.gz",
            "treemd-aarch64-unknown-linux-musl.sha256",
            "treemd-aarch64-unknown-linux-musl.tar.gz",
            "treemd-x86_64-apple-darwin.sha256",
            "treemd-x86_64-apple-darwin.tar.gz",
            "treemd-x86_64-pc-windows-msvc.exe.sha256",
            "treemd-x86_64-pc-windows-msvc.exe.zip",
            "treemd-x86_64-unknown-linux-gnu.sha256",
            "treemd-x86_64-unknown-linux-gnu.tar.gz",
            "treemd-x86_64-unknown-linux-musl.sha256",
            "treemd-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_epistates_treemd_treemd_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 14),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 8),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 10),
            ],
            &epistates_treemd_treemd_names(),
            "treemd",
        );
    }

    fn f1bonacc1_process_compose_process_compose_names() -> Vec<&'static str> {
        vec![
            "process-compose_checksums.txt",
            "process-compose_darwin_amd64.tar.gz",
            "process-compose_darwin_arm64.tar.gz",
            "process-compose_linux_386.tar.gz",
            "process-compose_linux_amd64.tar.gz",
            "process-compose_linux_arm.tar.gz",
            "process-compose_linux_arm64.tar.gz",
            "process-compose_windows_amd64.zip",
            "process-compose_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_f1bonacc1_process_compose_process_compose_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 3),
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 7),
                (Platform::WinArm64, 8),
            ],
            &f1bonacc1_process_compose_process_compose_names(),
            "process-compose",
        );
    }

    fn fairwindsops_rbac_lookup_rbac_lookup_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "checksums.txt.sig",
            "rbac-lookup_0.10.3_Darwin_arm64.tar.gz",
            "rbac-lookup_0.10.3_Darwin_x86_64.tar.gz",
            "rbac-lookup_0.10.3_Linux_arm64.tar.gz",
            "rbac-lookup_0.10.3_Linux_armv6.tar.gz",
            "rbac-lookup_0.10.3_Linux_armv7.tar.gz",
            "rbac-lookup_0.10.3_Linux_x86_64.tar.gz",
            "rbac-lookup_0.10.3_Windows_arm64.tar.gz",
            "rbac-lookup_0.10.3_Windows_armv6.tar.gz",
            "rbac-lookup_0.10.3_Windows_armv7.tar.gz",
            "rbac-lookup_0.10.3_Windows_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_fairwindsops_rbac_lookup_rbac_lookup_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 11),
                (Platform::WinArm64, 8),
            ],
            &fairwindsops_rbac_lookup_rbac_lookup_names(),
            "rbac-lookup",
        );
    }

    fn falconforceteam_falconhound_falconhound_names() -> Vec<&'static str> {
        vec![
            "FalconHound_Darwin_arm64.zip",
            "FalconHound_Darwin_x86_64.zip",
            "FalconHound_Linux_arm64.zip",
            "FalconHound_Linux_x86_64.zip",
            "FalconHound_Windows_arm64.zip",
            "FalconHound_Windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_falconforceteam_falconhound_falconhound_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 5),
                (Platform::WinArm64, 4),
            ],
            &falconforceteam_falconhound_falconhound_names(),
            "FalconHound",
        );
    }

    fn gaurav_gosain_tuios_tuios_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "tuios-web_0.6.0_Darwin_arm64.tar.gz",
            "tuios-web_0.6.0_Darwin_x86_64.tar.gz",
            "tuios-web_0.6.0_Freebsd_arm64.tar.gz",
            "tuios-web_0.6.0_Freebsd_i386.tar.gz",
            "tuios-web_0.6.0_Freebsd_x86_64.tar.gz",
            "tuios-web_0.6.0_Linux_arm64.tar.gz",
            "tuios-web_0.6.0_Linux_armv6.tar.gz",
            "tuios-web_0.6.0_Linux_armv7.tar.gz",
            "tuios-web_0.6.0_Linux_i386.tar.gz",
            "tuios-web_0.6.0_Linux_x86_64.tar.gz",
            "tuios-web_0.6.0_Openbsd_arm64.tar.gz",
            "tuios-web_0.6.0_Openbsd_i386.tar.gz",
            "tuios-web_0.6.0_Openbsd_x86_64.tar.gz",
            "tuios-web_0.6.0_Windows_i386.tar.gz",
            "tuios-web_0.6.0_Windows_x86_64.tar.gz",
            "tuios_0.6.0_Darwin_arm64.tar.gz",
            "tuios_0.6.0_Darwin_x86_64.tar.gz",
            "tuios_0.6.0_Freebsd_arm64.tar.gz",
            "tuios_0.6.0_Freebsd_i386.tar.gz",
            "tuios_0.6.0_Freebsd_x86_64.tar.gz",
            "tuios_0.6.0_Linux_arm64.tar.gz",
            "tuios_0.6.0_Linux_armv6.tar.gz",
            "tuios_0.6.0_Linux_armv7.tar.gz",
            "tuios_0.6.0_Linux_i386.tar.gz",
            "tuios_0.6.0_Linux_x86_64.tar.gz",
            "tuios_0.6.0_Openbsd_arm64.tar.gz",
            "tuios_0.6.0_Openbsd_i386.tar.gz",
            "tuios_0.6.0_Openbsd_x86_64.tar.gz",
            "tuios_0.6.0_Windows_i386.tar.gz",
            "tuios_0.6.0_Windows_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_gaurav_gosain_tuios_tuios_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 25),
                (Platform::LinuxAarch64, 21),
                (Platform::Osx64, 17),
                (Platform::OsxArm64, 16),
                (Platform::Win64, 30),
            ],
            &gaurav_gosain_tuios_tuios_names(),
            "tuios",
        );
    }

    fn getdeck_getdeck_deck_names() -> Vec<&'static str> {
        vec![
            "deck-0.11.1-darwin-universal.zip",
            "deck-0.11.1-linux-amd64.zip",
            "deck-0.11.1-windows-x86_64.zip",
        ]
    }

    #[test]
    fn test_getdeck_getdeck_deck_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 2),
            ],
            &getdeck_getdeck_deck_names(),
            "deck",
        );
    }

    fn ghosttroops_scan4all_scan4all_names() -> Vec<&'static str> {
        vec![
            "scan4all-linux-checksums.txt",
            "scan4all-mac-checksums.txt",
            "scan4all-windows-checksums.txt",
            "scan4all_2.9.1_darwin_amd64.zip",
            "scan4all_2.9.1_linux_amd64.zip",
            "scan4all_2.9.1_windows_amd64.zip",
        ]
    }

    #[test]
    fn test_ghosttroops_scan4all_scan4all_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::Win64, 5),
            ],
            &ghosttroops_scan4all_scan4all_names(),
            "scan4all",
        );
    }

    fn gitguardian_ggshield_ggshield_names() -> Vec<&'static str> {
        vec![
            "ggshield-1.48.0-1.x86_64.rpm",
            "ggshield-1.48.0-arm64-apple-darwin.pkg",
            "ggshield-1.48.0-arm64-apple-darwin.tar.gz",
            "ggshield-1.48.0-x86_64-apple-darwin.pkg",
            "ggshield-1.48.0-x86_64-apple-darwin.tar.gz",
            "ggshield-1.48.0-x86_64-pc-windows-msvc.zip",
            "ggshield-1.48.0-x86_64-unknown-linux-gnu.tar.gz",
            "ggshield.1.48.0.nupkg",
            "ggshield_1.48.0-1_amd64.deb",
        ]
    }

    #[test]
    fn test_gitguardian_ggshield_ggshield_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 5),
            ],
            &gitguardian_ggshield_ggshield_names(),
            "ggshield",
        );
    }

    fn ixday_mruby_mruby_names() -> Vec<&'static str> {
        vec![
            "mruby-linux-aarch64-musl.zip",
            "mruby-linux-aarch64.zip",
            "mruby-linux-x86_64-musl.zip",
            "mruby-linux-x86_64.zip",
            "mruby-macos-aarch64.zip",
        ]
    }

    #[test]
    fn test_ixday_mruby_mruby_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 1),
                (Platform::OsxArm64, 4),
            ],
            &ixday_mruby_mruby_names(),
            "mruby",
        );
    }

    fn johnnymorganz_stylua_stylua_names() -> Vec<&'static str> {
        vec![
            "stylua-linux-aarch64-musl.zip",
            "stylua-linux-aarch64.zip",
            "stylua-linux-x86_64-musl.zip",
            "stylua-linux-x86_64.zip",
            "stylua-macos-aarch64.zip",
            "stylua-macos-x86_64.zip",
            "stylua-windows-x86_64.zip",
        ]
    }

    #[test]
    fn test_johnnymorganz_stylua_stylua_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 6),
            ],
            &johnnymorganz_stylua_stylua_names(),
            "stylua",
        );
    }

    fn julien_r44_fast_ssh_fast_ssh_names() -> Vec<&'static str> {
        vec![
            "fast-ssh-v0.3.2-x86_64-apple-darwin.tar.gz",
            "fast-ssh-v0.3.2-x86_64-pc-windows-msvc.zip",
            "fast-ssh-v0.3.2-x86_64-unknown-linux-gnu.tar.gz",
        ]
    }

    #[test]
    fn test_julien_r44_fast_ssh_fast_ssh_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 0),
                (Platform::Win64, 1),
            ],
            &julien_r44_fast_ssh_fast_ssh_names(),
            "fast-ssh",
        );
    }

    fn kampfkarren_selene_selene_light_names() -> Vec<&'static str> {
        vec![
            "selene-0.30.0-linux.zip",
            "selene-0.30.0-macos.zip",
            "selene-0.30.0-windows.zip",
            "selene-light-0.30.0-linux.zip",
            "selene-light-0.30.0-macos.zip",
            "selene-light-0.30.0-windows.zip",
        ]
    }

    #[test]
    fn test_kampfkarren_selene_selene_light_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 5),
            ],
            &kampfkarren_selene_selene_light_names(),
            "selene-light",
        );
    }

    fn khronosgroup_ktx_software_ktx_software_names() -> Vec<&'static str> {
        vec![
            "KTX-Software-4.4.2-Android.zip",
            "KTX-Software-4.4.2-Android.zip.sha1",
            "KTX-Software-4.4.2-Darwin-arm64.pkg",
            "KTX-Software-4.4.2-Darwin-x86_64.pkg",
            "KTX-Software-4.4.2-iOS-arm64.zip",
            "KTX-Software-4.4.2-iOS-arm64.zip.sha1",
            "KTX-Software-4.4.2-Linux-arm64.deb",
            "KTX-Software-4.4.2-Linux-arm64.deb.sha1",
            "KTX-Software-4.4.2-Linux-arm64.rpm",
            "KTX-Software-4.4.2-Linux-arm64.rpm.sha1",
            "KTX-Software-4.4.2-Linux-arm64.tar.bz2",
            "KTX-Software-4.4.2-Linux-arm64.tar.bz2.sha1",
            "KTX-Software-4.4.2-Linux-x86_64.deb",
            "KTX-Software-4.4.2-Linux-x86_64.deb.sha1",
            "KTX-Software-4.4.2-Linux-x86_64.rpm",
            "KTX-Software-4.4.2-Linux-x86_64.rpm.sha1",
            "KTX-Software-4.4.2-Linux-x86_64.tar.bz2",
            "KTX-Software-4.4.2-Linux-x86_64.tar.bz2.sha1",
            "KTX-Software-4.4.2-Web-libktx.zip",
            "KTX-Software-4.4.2-Web-libktx.zip.sha1",
            "KTX-Software-4.4.2-Web-libktx_read.zip",
            "KTX-Software-4.4.2-Web-libktx_read.zip.sha1",
            "KTX-Software-4.4.2-Web-msc_basis_transcoder.zip",
            "KTX-Software-4.4.2-Web-msc_basis_transcoder.zip.sha1",
            "KTX-Software-4.4.2-Windows-arm64.exe",
            "KTX-Software-4.4.2-Windows-x64.exe",
            "pyktx-4.4.2-cp310-cp310-linux_aarch64.whl",
            "pyktx-4.4.2-cp310-cp310-linux_x86_64.whl",
            "pyktx-4.4.2-cp312-cp312-win_amd64.whl",
            "pyktx-4.4.2-cp312-cp312-win_arm64.whl",
            "pyktx-4.4.2-cp313-cp313-macosx_15_0_arm64.whl",
            "pyktx-4.4.2.tar.gz",
        ]
    }

    #[test]
    fn test_khronosgroup_ktx_software_ktx_software_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 16),
                (Platform::LinuxAarch64, 10),
            ],
            &khronosgroup_ktx_software_ktx_software_names(),
            "KTX-Software",
        );
    }

    fn kitware_cmake_cmake_names() -> Vec<&'static str> {
        vec![
            "cmake-4.2.3-files-v1.json",
            "cmake-4.2.3-linux-aarch64.sh",
            "cmake-4.2.3-linux-aarch64.tar.gz",
            "cmake-4.2.3-linux-x86_64.sh",
            "cmake-4.2.3-linux-x86_64.tar.gz",
            "cmake-4.2.3-macos-universal.dmg",
            "cmake-4.2.3-macos-universal.tar.gz",
            "cmake-4.2.3-macos10.10-universal.dmg",
            "cmake-4.2.3-macos10.10-universal.tar.gz",
            "cmake-4.2.3-SHA-256.txt",
            "cmake-4.2.3-SHA-256.txt.asc",
            "cmake-4.2.3-sunos-sparc64.sh",
            "cmake-4.2.3-sunos-sparc64.tar.gz",
            "cmake-4.2.3-sunos-x86_64.sh",
            "cmake-4.2.3-sunos-x86_64.tar.gz",
            "cmake-4.2.3-windows-arm64.msi",
            "cmake-4.2.3-windows-arm64.zip",
            "cmake-4.2.3-windows-i386.msi",
            "cmake-4.2.3-windows-i386.zip",
            "cmake-4.2.3-windows-x86_64.msi",
            "cmake-4.2.3-windows-x86_64.zip",
            "cmake-4.2.3.tar.gz",
            "cmake-4.2.3.zip",
        ]
    }

    #[test]
    fn test_kitware_cmake_cmake_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
            ],
            &kitware_cmake_cmake_names(),
            "cmake",
        );
    }

    fn kusionstack_kusion_kusion_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "kusion_0.15.0_darwin_amd64.tar.gz",
            "kusion_0.15.0_darwin_arm64.tar.gz",
            "kusion_0.15.0_linux_amd64.tar.gz",
            "kusion_0.15.0_windows_amd64.zip",
        ]
    }

    #[test]
    fn test_kusionstack_kusion_kusion_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 4),
            ],
            &kusionstack_kusion_kusion_names(),
            "kusion",
        );
    }

    fn lgug2z_komorebi_komorebi_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "komorebi-0.1.40-aarch64-pc-windows-msvc.zip",
            "komorebi-0.1.40-aarch64.msi",
            "komorebi-0.1.40-x86_64-pc-windows-msvc.zip",
            "komorebi-0.1.40-x86_64.msi",
        ]
    }

    #[test]
    fn test_lgug2z_komorebi_komorebi_names() {
        platform_match_test(
            &[
                (Platform::Win64, 3),
                (Platform::WinArm64, 1),
            ],
            &lgug2z_komorebi_komorebi_names(),
            "komorebi",
        );
    }

    fn lallassu_gorss_gorss_names() -> Vec<&'static str> {
        vec![
            "gorss_linux.tar.gz",
            "gorss_osx.tar.gz",
        ]
    }

    #[test]
    fn test_lallassu_gorss_gorss_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
            ],
            &lallassu_gorss_gorss_names(),
            "gorss",
        );
    }

    fn luals_lua_language_server_lua_language_server_names() -> Vec<&'static str> {
        vec![
            "lua-language-server-3.17.1-darwin-arm64.tar.gz",
            "lua-language-server-3.17.1-darwin-x64.tar.gz",
            "lua-language-server-3.17.1-linux-arm64.tar.gz",
            "lua-language-server-3.17.1-linux-x64.tar.gz",
            "lua-language-server-3.17.1-submodules.zip",
            "lua-language-server-3.17.1-win32-ia32.zip",
            "lua-language-server-3.17.1-win32-x64.zip",
        ]
    }

    #[test]
    fn test_luals_lua_language_server_lua_language_server_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 6),
            ],
            &luals_lua_language_server_lua_language_server_names(),
            "lua-language-server",
        );
    }

    fn macchina_cli_macchina_macchina_names() -> Vec<&'static str> {
        vec![
            "macchina-v6.4.0-android-aarch64.tar.gz",
            "macchina-v6.4.0-freebsd-x86_64.tar.gz",
            "macchina-v6.4.0-linux-gnu-aarch64.tar.gz",
            "macchina-v6.4.0-linux-gnu-x86_64.tar.gz",
            "macchina-v6.4.0-linux-gnueabihf-armv7.tar.gz",
            "macchina-v6.4.0-linux-musl-aarch64.tar.gz",
            "macchina-v6.4.0-linux-musl-x86_64.tar.gz",
            "macchina-v6.4.0-macos-aarch64.tar.gz",
            "macchina-v6.4.0-macos-x86_64.tar.gz",
            "macchina-v6.4.0-windows-aarch64.exe",
            "macchina-v6.4.0-windows-x86_64.exe",
        ]
    }

    #[test]
    fn test_macchina_cli_macchina_macchina_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 8),
                (Platform::OsxArm64, 7),
            ],
            &macchina_cli_macchina_macchina_names(),
            "macchina",
        );
    }

    fn maybejustjames_zephyr_zephyr_names() -> Vec<&'static str> {
        vec![
            "Linux.sha",
            "Linux.tar.gz",
            "macOS.sha",
            "macOS.tar.gz",
            "Windows.sha",
            "Windows.tar.gz",
        ]
    }

    #[test]
    fn test_maybejustjames_zephyr_zephyr_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 3),
                (Platform::Win32, 5),
                (Platform::Win64, 5),
                (Platform::WinArm64, 5),
            ],
            &maybejustjames_zephyr_zephyr_names(),
            "zephyr",
        );
    }

    fn melkeydev_go_blueprint_go_blueprint_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "go-blueprint_0.10.11_Darwin_all.tar.gz",
            "go-blueprint_0.10.11_Linux_arm64.tar.gz",
            "go-blueprint_0.10.11_Linux_x86_64.tar.gz",
            "go-blueprint_0.10.11_Windows_arm64.zip",
            "go-blueprint_0.10.11_Windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_melkeydev_go_blueprint_go_blueprint_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 5),
                (Platform::WinArm64, 4),
            ],
            &melkeydev_go_blueprint_go_blueprint_names(),
            "go-blueprint",
        );
    }

    fn mic_u_ecsher_ecsher_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "ecsher_Darwin_arm64.tar.gz",
            "ecsher_Darwin_x86_64.tar.gz",
            "ecsher_Linux_arm64.tar.gz",
            "ecsher_Linux_i386.tar.gz",
            "ecsher_Linux_x86_64.tar.gz",
            "ecsher_Windows_arm64.tar.gz",
            "ecsher_Windows_i386.tar.gz",
            "ecsher_Windows_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_mic_u_ecsher_ecsher_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 8),
                (Platform::WinArm64, 6),
            ],
            &mic_u_ecsher_ecsher_names(),
            "ecsher",
        );
    }

    fn mordechaihadad_bob_bob_names() -> Vec<&'static str> {
        vec![
            "bob-linux-arm-appimage.zip",
            "bob-linux-arm-openssl.zip",
            "bob-linux-arm.zip",
            "bob-linux-x86_64-appimage.zip",
            "bob-linux-x86_64-openssl.zip",
            "bob-linux-x86_64.zip",
            "bob-macos-arm-openssl.zip",
            "bob-macos-arm.zip",
            "bob-macos-x86_64-openssl.zip",
            "bob-macos-x86_64.zip",
            "bob-windows-x86_64-openssl.zip",
            "bob-windows-x86_64.zip",
        ]
    }

    #[test]
    fn test_mordechaihadad_bob_bob_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 7),
                (Platform::Win64, 11),
            ],
            &mordechaihadad_bob_bob_names(),
            "bob",
        );
    }

    fn myriad_dreamin_tinymist_tinymist_names() -> Vec<&'static str> {
        vec![
            "dist-manifest.json",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
            "tinymist-aarch64-apple-darwin.tar.gz",
            "tinymist-aarch64-apple-darwin.tar.gz.sha256",
            "tinymist-aarch64-pc-windows-msvc.zip",
            "tinymist-aarch64-pc-windows-msvc.zip.sha256",
            "tinymist-aarch64-unknown-linux-gnu.tar.gz",
            "tinymist-aarch64-unknown-linux-gnu.tar.gz.sha256",
            "tinymist-aarch64-unknown-linux-musl.tar.gz",
            "tinymist-aarch64-unknown-linux-musl.tar.gz.sha256",
            "tinymist-alpine-arm64",
            "tinymist-alpine-arm64.debug",
            "tinymist-alpine-arm64.vsix",
            "tinymist-alpine-x64",
            "tinymist-alpine-x64.debug",
            "tinymist-alpine-x64.vsix",
            "tinymist-arm-unknown-linux-gnueabihf.tar.gz",
            "tinymist-arm-unknown-linux-gnueabihf.tar.gz.sha256",
            "tinymist-arm-unknown-linux-musleabihf.tar.gz",
            "tinymist-arm-unknown-linux-musleabihf.tar.gz.sha256",
            "tinymist-armv7-unknown-linux-gnueabihf.tar.gz",
            "tinymist-armv7-unknown-linux-gnueabihf.tar.gz.sha256",
            "tinymist-armv7-unknown-linux-musleabihf.tar.gz",
            "tinymist-armv7-unknown-linux-musleabihf.tar.gz.sha256",
            "tinymist-completions.tar.gz",
            "tinymist-darwin-arm64",
            "tinymist-darwin-arm64.vsix",
            "tinymist-darwin-x64",
            "tinymist-darwin-x64.vsix",
            "tinymist-docs.pdf",
            "tinymist-installer.ps1",
            "tinymist-installer.sh",
            "tinymist-linux-arm64",
            "tinymist-linux-arm64.vsix",
            "tinymist-linux-armhf",
            "tinymist-linux-armhf.vsix",
            "tinymist-linux-x64",
            "tinymist-linux-x64.vsix",
            "tinymist-loongarch64-unknown-linux-gnu.tar.gz",
            "tinymist-loongarch64-unknown-linux-gnu.tar.gz.sha256",
            "tinymist-loongarch64-unknown-linux-musl.tar.gz",
            "tinymist-loongarch64-unknown-linux-musl.tar.gz.sha256",
            "tinymist-riscv64gc-unknown-linux-musl.tar.gz",
            "tinymist-riscv64gc-unknown-linux-musl.tar.gz.sha256",
            "tinymist-universal.vsix",
            "tinymist-web.tar.gz",
            "tinymist-web.vsix",
            "tinymist-win32-arm64.exe",
            "tinymist-win32-arm64.vsix",
            "tinymist-win32-x64.exe",
            "tinymist-win32-x64.vsix",
            "tinymist-x86_64-apple-darwin.tar.gz",
            "tinymist-x86_64-apple-darwin.tar.gz.sha256",
            "tinymist-x86_64-pc-windows-msvc.zip",
            "tinymist-x86_64-pc-windows-msvc.zip.sha256",
            "tinymist-x86_64-unknown-linux-gnu.tar.gz",
            "tinymist-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "tinymist-x86_64-unknown-linux-musl.tar.gz",
            "tinymist-x86_64-unknown-linux-musl.tar.gz.sha256",
            "typlite-aarch64-apple-darwin.tar.gz",
            "typlite-aarch64-apple-darwin.tar.gz.sha256",
            "typlite-aarch64-pc-windows-msvc.zip",
            "typlite-aarch64-pc-windows-msvc.zip.sha256",
            "typlite-aarch64-unknown-linux-gnu.tar.gz",
            "typlite-aarch64-unknown-linux-gnu.tar.gz.sha256",
            "typlite-aarch64-unknown-linux-musl.tar.gz",
            "typlite-aarch64-unknown-linux-musl.tar.gz.sha256",
            "typlite-arm-unknown-linux-gnueabihf.tar.gz",
            "typlite-arm-unknown-linux-gnueabihf.tar.gz.sha256",
            "typlite-arm-unknown-linux-musleabihf.tar.gz",
            "typlite-arm-unknown-linux-musleabihf.tar.gz.sha256",
            "typlite-armv7-unknown-linux-gnueabihf.tar.gz",
            "typlite-armv7-unknown-linux-gnueabihf.tar.gz.sha256",
            "typlite-armv7-unknown-linux-musleabihf.tar.gz",
            "typlite-armv7-unknown-linux-musleabihf.tar.gz.sha256",
            "typlite-installer.ps1",
            "typlite-installer.sh",
            "typlite-loongarch64-unknown-linux-gnu.tar.gz",
            "typlite-loongarch64-unknown-linux-gnu.tar.gz.sha256",
            "typlite-loongarch64-unknown-linux-musl.tar.gz",
            "typlite-loongarch64-unknown-linux-musl.tar.gz.sha256",
            "typlite-riscv64gc-unknown-linux-musl.tar.gz",
            "typlite-riscv64gc-unknown-linux-musl.tar.gz.sha256",
            "typlite-x86_64-apple-darwin.tar.gz",
            "typlite-x86_64-apple-darwin.tar.gz.sha256",
            "typlite-x86_64-pc-windows-msvc.zip",
            "typlite-x86_64-pc-windows-msvc.zip.sha256",
            "typlite-x86_64-unknown-linux-gnu.tar.gz",
            "typlite-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "typlite-x86_64-unknown-linux-musl.tar.gz",
            "typlite-x86_64-unknown-linux-musl.tar.gz.sha256",
            "typst-preview-alpine-arm64.vsix",
            "typst-preview-alpine-x64.vsix",
            "typst-preview-darwin-arm64.vsix",
            "typst-preview-darwin-x64.vsix",
            "typst-preview-linux-arm64.vsix",
            "typst-preview-linux-armhf.vsix",
            "typst-preview-linux-x64.vsix",
            "typst-preview-win32-arm64.vsix",
            "typst-preview-win32-x64.vsix",
        ]
    }

    #[test]
    fn test_myriad_dreamin_tinymist_tinymist_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 38),
                (Platform::LinuxAarch64, 34),
                (Platform::Osx64, 29),
                (Platform::OsxArm64, 27),
            ],
            &myriad_dreamin_tinymist_tinymist_names(),
            "tinymist",
        );
    }

    fn nikitacoeur_dirvana_dirvana_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "dirvana_0.8.2_Darwin_arm64.tar.gz",
            "dirvana_0.8.2_Darwin_x86_64.tar.gz",
            "dirvana_0.8.2_Linux_arm64.tar.gz",
            "dirvana_0.8.2_Linux_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_nikitacoeur_dirvana_dirvana_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
            ],
            &nikitacoeur_dirvana_dirvana_names(),
            "dirvana",
        );
    }

    fn nukesor_pueue_pueue_names() -> Vec<&'static str> {
        vec![
            "pueue-aarch64-apple-darwin",
            "pueue-aarch64-unknown-linux-musl",
            "pueue-arm-unknown-linux-musleabihf",
            "pueue-armv7-unknown-linux-musleabihf",
            "pueue-x86_64-apple-darwin",
            "pueue-x86_64-pc-windows-msvc.exe",
            "pueue-x86_64-unknown-freebsd",
            "pueue-x86_64-unknown-linux-musl",
            "pueued-aarch64-apple-darwin",
            "pueued-aarch64-unknown-linux-musl",
            "pueued-arm-unknown-linux-musleabihf",
            "pueued-armv7-unknown-linux-musleabihf",
            "pueued-x86_64-apple-darwin",
            "pueued-x86_64-pc-windows-msvc.exe",
            "pueued-x86_64-unknown-freebsd",
            "pueued-x86_64-unknown-linux-musl",
            "systemd.pueued.service",
        ]
    }

    #[test]
    fn test_nukesor_pueue_pueue_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 0),
            ],
            &nukesor_pueue_pueue_names(),
            "pueue",
        );
    }

    fn orange_opensource_hurl_hurl_names() -> Vec<&'static str> {
        vec![
            "hurl-7.1.0-aarch64-apple-darwin.tar.gz",
            "hurl-7.1.0-aarch64-apple-darwin.tar.gz.sha256",
            "hurl-7.1.0-aarch64-unknown-linux-gnu.tar.gz",
            "hurl-7.1.0-aarch64-unknown-linux-gnu.tar.gz.sha256",
            "hurl-7.1.0-x86_64-apple-darwin.tar.gz",
            "hurl-7.1.0-x86_64-apple-darwin.tar.gz.sha256",
            "hurl-7.1.0-x86_64-pc-windows-msvc-installer.exe",
            "hurl-7.1.0-x86_64-pc-windows-msvc-installer.exe.sha256",
            "hurl-7.1.0-x86_64-pc-windows-msvc.zip",
            "hurl-7.1.0-x86_64-pc-windows-msvc.zip.sha256",
            "hurl-7.1.0-x86_64-unknown-linux-gnu.tar.gz",
            "hurl-7.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "hurl_7.1.0_amd64.deb",
            "hurl_7.1.0_amd64.deb.sha256",
            "hurl_7.1.0_arm64.deb",
            "hurl_7.1.0_arm64.deb.sha256",
        ]
    }

    #[test]
    fn test_orange_opensource_hurl_hurl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 8),
            ],
            &orange_opensource_hurl_hurl_names(),
            "hurl",
        );
    }

    fn owloops_updo_updo_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "updo_0.4.6_linux_amd64.apk",
            "updo_0.4.6_linux_amd64.deb",
            "updo_0.4.6_linux_amd64.pkg.tar.zst",
            "updo_0.4.6_linux_amd64.rpm",
            "updo_0.4.6_linux_arm64.apk",
            "updo_0.4.6_linux_arm64.deb",
            "updo_0.4.6_linux_arm64.pkg.tar.zst",
            "updo_0.4.6_linux_arm64.rpm",
            "updo_Darwin_arm64",
            "updo_Darwin_x86_64",
            "updo_Linux_arm64",
            "updo_Linux_x86_64",
            "updo_Windows_arm64.exe",
            "updo_Windows_x86_64.exe",
        ]
    }

    #[test]
    fn test_owloops_updo_updo_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 12),
                (Platform::LinuxAarch64, 11),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 9),
            ],
            &owloops_updo_updo_names(),
            "updo",
        );
    }

    fn pauljuliusmartinez_jless_jless_names() -> Vec<&'static str> {
        vec![
            "jless-v0.9.0-aarch64-apple-darwin.zip",
            "jless-v0.9.0-x86_64-apple-darwin.zip",
            "jless-v0.9.0-x86_64-unknown-linux-gnu.zip",
        ]
    }

    #[test]
    fn test_pauljuliusmartinez_jless_jless_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
            ],
            &pauljuliusmartinez_jless_jless_names(),
            "jless",
        );
    }

    fn percona_lab_mysql_random_data_load_mysql_random_data_load_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "mysql_random_data_load_0.1.12_Darwin_i386.tar.gz",
            "mysql_random_data_load_0.1.12_Darwin_x86_64.tar.gz",
            "mysql_random_data_load_0.1.12_Linux_i386.tar.gz",
            "mysql_random_data_load_0.1.12_Linux_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_percona_lab_mysql_random_data_load_mysql_random_data_load_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::Osx64, 2),
            ],
            &percona_lab_mysql_random_data_load_mysql_random_data_load_names(),
            "mysql_random_data_load",
        );
    }

    fn phantas0s_devdash_devdash_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "devdash_0.5.0_Darwin_x86_64.tar.gz",
            "devdash_0.5.0_Linux_arm64.tar.gz",
            "devdash_0.5.0_Linux_x86.tar.gz",
            "devdash_0.5.0_Linux_x86_64.tar.gz",
            "devdash_0.5.0_Windows_x86.zip",
            "devdash_0.5.0_Windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_phantas0s_devdash_devdash_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 1),
                (Platform::Win64, 6),
            ],
            &phantas0s_devdash_devdash_names(),
            "devdash",
        );
    }

    fn piturnah_gex_gex_names() -> Vec<&'static str> {
        vec![
            "gex-x86_64-apple-darwin.tar.gz",
            "gex-x86_64-pc-windows-msvc.zip",
            "gex-x86_64-unknown-linux-gnu.tar.gz",
        ]
    }

    #[test]
    fn test_piturnah_gex_gex_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 0),
                (Platform::Win64, 1),
            ],
            &piturnah_gex_gex_names(),
            "gex",
        );
    }

    fn powershell_powershell_powershell_names() -> Vec<&'static str> {
        vec![
            "hashes.sha256",
            "powershell-7.5.4-1.cm.aarch64.rpm",
            "powershell-7.5.4-1.cm.x86_64.rpm",
            "powershell-7.5.4-1.rh.x86_64.rpm",
            "powershell-7.5.4-linux-arm32.tar.gz",
            "powershell-7.5.4-linux-arm64.tar.gz",
            "powershell-7.5.4-linux-musl-x64.tar.gz",
            "powershell-7.5.4-linux-x64-fxdependent.tar.gz",
            "powershell-7.5.4-linux-x64-musl-noopt-fxdependent.tar.gz",
            "powershell-7.5.4-linux-x64.tar.gz",
            "powershell-7.5.4-osx-arm64.pkg",
            "powershell-7.5.4-osx-arm64.tar.gz",
            "powershell-7.5.4-osx-x64.pkg",
            "powershell-7.5.4-osx-x64.tar.gz",
            "PowerShell-7.5.4-win-arm64.msi",
            "PowerShell-7.5.4-win-arm64.zip",
            "PowerShell-7.5.4-win-fxdependent.zip",
            "PowerShell-7.5.4-win-fxdependentWinDesktop.zip",
            "PowerShell-7.5.4-win-x64.msi",
            "PowerShell-7.5.4-win-x64.zip",
            "PowerShell-7.5.4-win-x86.msi",
            "PowerShell-7.5.4-win-x86.zip",
            "PowerShell-7.5.4.msixbundle",
            "powershell_7.5.4-1.deb_amd64.deb",
        ]
    }

    #[test]
    fn test_powershell_powershell_powershell_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 13),
                (Platform::OsxArm64, 11),
                (Platform::Win64, 19),
                (Platform::WinArm64, 15),
            ],
            &powershell_powershell_powershell_names(),
            "powershell",
        );
    }

    fn qovery_replibyte_replibyte_names() -> Vec<&'static str> {
        vec![
            "replibyte_v0.10.0_x86_64-apple-darwin.zip",
            "replibyte_v0.10.0_x86_64-apple-darwin.zip.sha256sum",
            "replibyte_v0.10.0_x86_64-pc-windows-gnu.exe.zip",
            "replibyte_v0.10.0_x86_64-pc-windows-gnu.exe.zip.sha256sum",
            "replibyte_v0.10.0_x86_64-unknown-linux-musl.tar.gz",
            "replibyte_v0.10.0_x86_64-unknown-linux-musl.tar.gz.sha256sum",
        ]
    }

    #[test]
    fn test_qovery_replibyte_replibyte_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::Osx64, 0),
                (Platform::Win64, 2),
            ],
            &qovery_replibyte_replibyte_names(),
            "replibyte",
        );
    }

    fn rigellute_spotify_tui_spotify_tui_names() -> Vec<&'static str> {
        vec![
            "spotify-tui-linux.sha256",
            "spotify-tui-linux.tar.gz",
            "spotify-tui-macos.sha256",
            "spotify-tui-macos.tar.gz",
            "spotify-tui-windows.sha256",
            "spotify-tui-windows.tar.gz",
        ]
    }

    #[test]
    fn test_rigellute_spotify_tui_spotify_tui_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 5),
            ],
            &rigellute_spotify_tui_spotify_tui_names(),
            "spotify-tui",
        );
    }

    fn supercilex_fuc_cpz_names() -> Vec<&'static str> {
        vec![
            "aarch64-apple-darwin-cpz",
            "aarch64-apple-darwin-rmz",
            "aarch64-pc-windows-msvc-cpz.exe",
            "aarch64-pc-windows-msvc-rmz.exe",
            "aarch64-unknown-linux-gnu-cpz",
            "aarch64-unknown-linux-gnu-rmz",
            "riscv64gc-unknown-linux-gnu-cpz",
            "riscv64gc-unknown-linux-gnu-rmz",
            "x86_64-apple-darwin-cpz",
            "x86_64-apple-darwin-rmz",
            "x86_64-pc-windows-msvc-cpz.exe",
            "x86_64-pc-windows-msvc-rmz.exe",
            "x86_64-unknown-linux-gnu-cpz",
            "x86_64-unknown-linux-gnu-rmz",
            "x86_64-unknown-linux-musl-cpz",
            "x86_64-unknown-linux-musl-rmz",
        ]
    }

    #[test]
    fn test_supercilex_fuc_cpz_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 14),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 8),
                (Platform::OsxArm64, 0),
            ],
            &supercilex_fuc_cpz_names(),
            "cpz",
        );
    }

    fn supercilex_fuc_rmz_names() -> Vec<&'static str> {
        vec![
            "aarch64-apple-darwin-cpz",
            "aarch64-apple-darwin-rmz",
            "aarch64-pc-windows-msvc-cpz.exe",
            "aarch64-pc-windows-msvc-rmz.exe",
            "aarch64-unknown-linux-gnu-cpz",
            "aarch64-unknown-linux-gnu-rmz",
            "riscv64gc-unknown-linux-gnu-cpz",
            "riscv64gc-unknown-linux-gnu-rmz",
            "x86_64-apple-darwin-cpz",
            "x86_64-apple-darwin-rmz",
            "x86_64-pc-windows-msvc-cpz.exe",
            "x86_64-pc-windows-msvc-rmz.exe",
            "x86_64-unknown-linux-gnu-cpz",
            "x86_64-unknown-linux-gnu-rmz",
            "x86_64-unknown-linux-musl-cpz",
            "x86_64-unknown-linux-musl-rmz",
        ]
    }

    #[test]
    fn test_supercilex_fuc_rmz_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 15),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 1),
            ],
            &supercilex_fuc_rmz_names(),
            "rmz",
        );
    }

    fn shopify_shadowenv_shadowenv_names() -> Vec<&'static str> {
        vec![
            "shadowenv-aarch64-apple-darwin",
            "shadowenv-aarch64-unknown-linux-gnu",
            "shadowenv-x86_64-apple-darwin",
            "shadowenv-x86_64-unknown-linux-gnu",
        ]
    }

    #[test]
    fn test_shopify_shadowenv_shadowenv_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 0),
            ],
            &shopify_shadowenv_shadowenv_names(),
            "shadowenv",
        );
    }

    fn skardyy_mcat_mcat_names() -> Vec<&'static str> {
        vec![
            "dist-manifest.json",
            "mcat-aarch64-apple-darwin.tar.xz",
            "mcat-aarch64-apple-darwin.tar.xz.sha256",
            "mcat-aarch64-unknown-linux-gnu.tar.xz",
            "mcat-aarch64-unknown-linux-gnu.tar.xz.sha256",
            "mcat-installer.ps1",
            "mcat-installer.sh",
            "mcat-x86_64-apple-darwin.tar.xz",
            "mcat-x86_64-apple-darwin.tar.xz.sha256",
            "mcat-x86_64-pc-windows-msvc.msi",
            "mcat-x86_64-pc-windows-msvc.msi.sha256",
            "mcat-x86_64-pc-windows-msvc.zip",
            "mcat-x86_64-pc-windows-msvc.zip.sha256",
            "mcat-x86_64-unknown-linux-gnu.tar.xz",
            "mcat-x86_64-unknown-linux-gnu.tar.xz.sha256",
            "mcat.rb",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_skardyy_mcat_mcat_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 13),
                (Platform::LinuxAarch64, 3),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 11),
            ],
            &skardyy_mcat_mcat_names(),
            "mcat",
        );
    }

    fn supercuber_dotter_dotter_names() -> Vec<&'static str> {
        vec![
            "completions.zip",
            "dotter-linux-arm64-musl",
            "dotter-linux-x64-musl",
            "dotter-macos-arm64.arm",
            "dotter-windows-x64-msvc.exe",
        ]
    }

    #[test]
    fn test_supercuber_dotter_dotter_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 1),
                (Platform::OsxArm64, 3),
            ],
            &supercuber_dotter_dotter_names(),
            "dotter",
        );
    }

    fn tako8ki_frum_frum_names() -> Vec<&'static str> {
        vec![
            "frum-v0.1.2-arm-unknown-linux-gnueabihf.tar.gz",
            "frum-v0.1.2-x86_64-apple-darwin.tar.gz",
            "frum-v0.1.2-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_tako8ki_frum_frum_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 1),
            ],
            &tako8ki_frum_frum_names(),
            "frum",
        );
    }

    fn tako8ki_gobang_gobang_names() -> Vec<&'static str> {
        vec![
            "gobang-0.1.0-alpha.5-arm-unknown-linux-gnueabihf.tar.gz",
            "gobang-0.1.0-alpha.5-i686-pc-windows-msvc.zip",
            "gobang-0.1.0-alpha.5-x86_64-apple-darwin.tar.gz",
            "gobang-0.1.0-alpha.5-x86_64-pc-windows-msvc.zip",
            "gobang-0.1.0-alpha.5-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_tako8ki_gobang_gobang_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::Osx64, 2),
                (Platform::Win64, 3),
            ],
            &tako8ki_gobang_gobang_names(),
            "gobang",
        );
    }

    fn thezoraiz_ascii_image_converter_ascii_image_converter_names() -> Vec<&'static str> {
        vec![
            "ascii-image-converter_Linux_amd64_64bit.tar.gz",
            "ascii-image-converter_Linux_arm64_64bit.tar.gz",
            "ascii-image-converter_Linux_armv6_32bit.tar.gz",
            "ascii-image-converter_Linux_i386_32bit.tar.gz",
            "ascii-image-converter_macOS_amd64_64bit.tar.gz",
            "ascii-image-converter_macOS_arm64_64bit.tar.gz",
            "ascii-image-converter_Windows_amd64_64bit.zip",
            "ascii-image-converter_Windows_arm64_64bit.zip",
            "ascii-image-converter_Windows_armv6_32bit.zip",
            "ascii-image-converter_Windows_i386_32bit.zip",
            "sha256_checksums.txt",
        ]
    }

    #[test]
    fn test_thezoraiz_ascii_image_converter_ascii_image_converter_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 3),
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 5),
                (Platform::Win32, 9),
                (Platform::Win64, 6),
                (Platform::WinArm64, 7),
            ],
            &thezoraiz_ascii_image_converter_ascii_image_converter_names(),
            "ascii-image-converter",
        );
    }

    fn trendyol_kink_kink_names() -> Vec<&'static str> {
        vec![
            "kink_0.2.1_Darwin-arm64.tar.gz",
            "kink_0.2.1_Darwin-x86_64.tar.gz",
            "kink_0.2.1_Linux-x86_64.tar.gz",
            "kink_0.2.1_Windows-x86_64.tar.gz",
            "kink_checksums.txt",
            "kink_checksums.txt.sig",
        ]
    }

    #[test]
    fn test_trendyol_kink_kink_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 3),
            ],
            &trendyol_kink_kink_names(),
            "kink",
        );
    }

    fn upcloudltd_upcloud_cli_upcloud_cli_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "upcloud-cli-3.30.0-1.aarch64.rpm",
            "upcloud-cli-3.30.0-1.x86_64.rpm",
            "upcloud-cli_3.30.0_aarch64.apk",
            "upcloud-cli_3.30.0_amd64.deb",
            "upcloud-cli_3.30.0_arm64.deb",
            "upcloud-cli_3.30.0_darwin_arm64.tar.gz",
            "upcloud-cli_3.30.0_darwin_arm64.tar.gz.spdx.json",
            "upcloud-cli_3.30.0_darwin_x86_64.tar.gz",
            "upcloud-cli_3.30.0_darwin_x86_64.tar.gz.spdx.json",
            "upcloud-cli_3.30.0_freebsd_arm64.tar.gz",
            "upcloud-cli_3.30.0_freebsd_arm64.tar.gz.spdx.json",
            "upcloud-cli_3.30.0_freebsd_x86_64.tar.gz",
            "upcloud-cli_3.30.0_freebsd_x86_64.tar.gz.spdx.json",
            "upcloud-cli_3.30.0_linux_arm64.tar.gz",
            "upcloud-cli_3.30.0_linux_arm64.tar.gz.spdx.json",
            "upcloud-cli_3.30.0_linux_x86_64.tar.gz",
            "upcloud-cli_3.30.0_linux_x86_64.tar.gz.spdx.json",
            "upcloud-cli_3.30.0_windows_arm64.zip",
            "upcloud-cli_3.30.0_windows_arm64.zip.spdx.json",
            "upcloud-cli_3.30.0_windows_x86_64.zip",
            "upcloud-cli_3.30.0_windows_x86_64.zip.spdx.json",
            "upcloud-cli_3.30.0_x86_64.apk",
        ]
    }

    #[test]
    fn test_upcloudltd_upcloud_cli_upcloud_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 16),
                (Platform::LinuxAarch64, 14),
                (Platform::Osx64, 8),
                (Platform::OsxArm64, 6),
                (Platform::Win64, 20),
                (Platform::WinArm64, 18),
            ],
            &upcloudltd_upcloud_cli_upcloud_cli_names(),
            "upcloud-cli",
        );
    }

    fn webassembly_binaryen_binaryen_names() -> Vec<&'static str> {
        vec![
            "binaryen-version_126-aarch64-linux.tar.gz",
            "binaryen-version_126-aarch64-linux.tar.gz.sha256",
            "binaryen-version_126-arm64-macos.tar.gz",
            "binaryen-version_126-arm64-macos.tar.gz.sha256",
            "binaryen-version_126-arm64-windows.tar.gz",
            "binaryen-version_126-arm64-windows.tar.gz.sha256",
            "binaryen-version_126-node.tar.gz",
            "binaryen-version_126-node.tar.gz.sha256",
            "binaryen-version_126-x86_64-linux.tar.gz",
            "binaryen-version_126-x86_64-linux.tar.gz.sha256",
            "binaryen-version_126-x86_64-macos.tar.gz",
            "binaryen-version_126-x86_64-macos.tar.gz.sha256",
            "binaryen-version_126-x86_64-windows.tar.gz",
            "binaryen-version_126-x86_64-windows.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_webassembly_binaryen_binaryen_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::LinuxAarch64, 0),
            ],
            &webassembly_binaryen_binaryen_names(),
            "binaryen",
        );
    }

    fn wilfred_difftastic_difft_names() -> Vec<&'static str> {
        vec![
            "difft-aarch64-apple-darwin.tar.gz",
            "difft-aarch64-pc-windows-msvc.zip",
            "difft-aarch64-unknown-linux-gnu.tar.gz",
            "difft-x86_64-apple-darwin.tar.gz",
            "difft-x86_64-pc-windows-msvc.zip",
            "difft-x86_64-unknown-linux-gnu.tar.gz",
            "difft-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_wilfred_difftastic_difft_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 4),
                (Platform::WinArm64, 1),
            ],
            &wilfred_difftastic_difft_names(),
            "difft",
        );
    }

    fn a8m_envsubst_envsubst_names() -> Vec<&'static str> {
        vec![
            "envsubst-Darwin-arm64",
            "envsubst-Darwin-arm64.md5",
            "envsubst-Darwin-x86_64",
            "envsubst-Darwin-x86_64.md5",
            "envsubst-Linux-arm64",
            "envsubst-Linux-arm64.md5",
            "envsubst-Linux-x86_64",
            "envsubst-Linux-x86_64.md5",
            "envsubst.exe",
            "envsubst.exe.md5",
        ]
    }

    #[test]
    fn test_a8m_envsubst_envsubst_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 0),
            ],
            &a8m_envsubst_envsubst_names(),
            "envsubst",
        );
    }

    fn aakso_ssh_inscribe_sshi_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "ssh-inscribe-0.11.0.x86_64.rpm",
            "ssh-inscribe-darwin-arm64",
            "ssh-inscribe-darwin-x86_64",
            "ssh-inscribe-linux-x86_64",
            "ssh-inscribe_0.11.0_amd64.deb",
            "sshi-0.11.0.x86_64.rpm",
            "sshi-darwin-arm64",
            "sshi-darwin-x86_64",
            "sshi-linux-x86_64",
            "sshi-windows-x86_64.exe",
            "sshi_0.11.0_amd64.deb",
        ]
    }

    #[test]
    fn test_aakso_ssh_inscribe_sshi_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::Osx64, 8),
                (Platform::OsxArm64, 7),
            ],
            &aakso_ssh_inscribe_sshi_names(),
            "sshi",
        );
    }

    fn aandrew_me_tgpt_tgpt_names() -> Vec<&'static str> {
        vec![
            "tgpt-amd64.exe",
            "tgpt-arm.exe",
            "tgpt-arm64.exe",
            "tgpt-freebsd-amd64",
            "tgpt-freebsd-arm",
            "tgpt-freebsd-arm64",
            "tgpt-freebsd-i386",
            "tgpt-i386.exe",
            "tgpt-linux-amd64",
            "tgpt-linux-arm",
            "tgpt-linux-arm64",
            "tgpt-linux-i386",
            "tgpt-mac-amd64",
            "tgpt-mac-arm64",
            "tgpt-netbsd-amd64",
            "tgpt-netbsd-arm",
            "tgpt-netbsd-arm64",
            "tgpt-netbsd-i386",
        ]
    }

    #[test]
    fn test_aandrew_me_tgpt_tgpt_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::LinuxAarch64, 10),
                (Platform::Osx64, 12),
                (Platform::OsxArm64, 13),
                (Platform::Win64, 0),
                (Platform::WinArm64, 2),
            ],
            &aandrew_me_tgpt_tgpt_names(),
            "tgpt",
        );
    }

    fn abhimanyu003_sttr_sttr_names() -> Vec<&'static str> {
        vec![
            "sttr_0.2.30_checksums.txt",
            "sttr_0.2.30_checksums.txt.sigstore.json",
            "sttr_0.2.30_linux_386.deb",
            "sttr_0.2.30_linux_386.pkg.tar.zst",
            "sttr_0.2.30_linux_386.rpm",
            "sttr_0.2.30_linux_amd64.deb",
            "sttr_0.2.30_linux_amd64.pkg.tar.zst",
            "sttr_0.2.30_linux_amd64.rpm",
            "sttr_0.2.30_linux_arm64.deb",
            "sttr_0.2.30_linux_arm64.pkg.tar.zst",
            "sttr_0.2.30_linux_arm64.rpm",
            "sttr_Darwin_all.tar.gz",
            "sttr_Darwin_all.tar.gz.sbom.json",
            "sttr_Freebsd_arm64.tar.gz",
            "sttr_Freebsd_arm64.tar.gz.sbom.json",
            "sttr_Freebsd_i386.tar.gz",
            "sttr_Freebsd_i386.tar.gz.sbom.json",
            "sttr_Freebsd_x86_64.tar.gz",
            "sttr_Freebsd_x86_64.tar.gz.sbom.json",
            "sttr_Linux_arm64.tar.gz",
            "sttr_Linux_arm64.tar.gz.sbom.json",
            "sttr_Linux_i386.tar.gz",
            "sttr_Linux_i386.tar.gz.sbom.json",
            "sttr_Linux_x86_64.tar.gz",
            "sttr_Linux_x86_64.tar.gz.sbom.json",
            "sttr_Windows_arm64.zip",
            "sttr_Windows_arm64.zip.sbom.json",
            "sttr_Windows_i386.zip",
            "sttr_Windows_i386.zip.sbom.json",
            "sttr_Windows_x86_64.zip",
            "sttr_Windows_x86_64.zip.sbom.json",
        ]
    }

    #[test]
    fn test_abhimanyu003_sttr_sttr_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 23),
                (Platform::LinuxAarch64, 19),
                (Platform::Osx64, 11),
                (Platform::OsxArm64, 11),
                (Platform::Win64, 29),
                (Platform::WinArm64, 25),
            ],
            &abhimanyu003_sttr_sttr_names(),
            "sttr",
        );
    }

    fn abiosoft_colima_colima_names() -> Vec<&'static str> {
        vec![
            "colima-Darwin-arm64",
            "colima-Darwin-arm64.sha256sum",
            "colima-Darwin-x86_64",
            "colima-Darwin-x86_64.sha256sum",
            "colima-Linux-aarch64",
            "colima-Linux-aarch64.sha256sum",
            "colima-Linux-x86_64",
            "colima-Linux-x86_64.sha256sum",
        ]
    }

    #[test]
    fn test_abiosoft_colima_colima_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 0),
            ],
            &abiosoft_colima_colima_names(),
            "colima",
        );
    }

    fn acheronfail_repgrep_repgrep_names() -> Vec<&'static str> {
        vec![
            "repgrep-0.16.1-arm-unknown-linux-gnueabihf.tar.gz",
            "repgrep-0.16.1-i686-pc-windows-msvc.zip",
            "repgrep-0.16.1-x86_64-apple-darwin.tar.gz",
            "repgrep-0.16.1-x86_64-pc-windows-gnu.zip",
            "repgrep-0.16.1-x86_64-pc-windows-msvc.zip",
            "repgrep-0.16.1-x86_64-unknown-linux-gnu.tar.gz",
            "repgrep-0.16.1-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_acheronfail_repgrep_repgrep_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::Osx64, 2),
                (Platform::Win64, 4),
            ],
            &acheronfail_repgrep_repgrep_names(),
            "repgrep",
        );
    }

    fn achristmascarl_rainfrog_rainfrog_names() -> Vec<&'static str> {
        vec![
            "rainfrog-v0.3.17-aarch64-apple-darwin.sha256",
            "rainfrog-v0.3.17-aarch64-apple-darwin.tar.gz",
            "rainfrog-v0.3.17-aarch64-linux-android.sha256",
            "rainfrog-v0.3.17-aarch64-linux-android.tar.gz",
            "rainfrog-v0.3.17-aarch64-unknown-linux-gnu.sha256",
            "rainfrog-v0.3.17-aarch64-unknown-linux-gnu.tar.gz",
            "rainfrog-v0.3.17-aarch64-unknown-linux-musl.sha256",
            "rainfrog-v0.3.17-aarch64-unknown-linux-musl.tar.gz",
            "rainfrog-v0.3.17-i686-unknown-linux-gnu.sha256",
            "rainfrog-v0.3.17-i686-unknown-linux-gnu.tar.gz",
            "rainfrog-v0.3.17-i686-unknown-linux-musl.sha256",
            "rainfrog-v0.3.17-i686-unknown-linux-musl.tar.gz",
            "rainfrog-v0.3.17-x86_64-apple-darwin.sha256",
            "rainfrog-v0.3.17-x86_64-apple-darwin.tar.gz",
            "rainfrog-v0.3.17-x86_64-pc-windows-msvc.sha256",
            "rainfrog-v0.3.17-x86_64-pc-windows-msvc.tar.gz",
            "rainfrog-v0.3.17-x86_64-unknown-linux-gnu.sha256",
            "rainfrog-v0.3.17-x86_64-unknown-linux-gnu.tar.gz",
            "rainfrog-v0.3.17-x86_64-unknown-linux-musl.sha256",
            "rainfrog-v0.3.17-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_achristmascarl_rainfrog_rainfrog_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 17),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 13),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 15),
            ],
            &achristmascarl_rainfrog_rainfrog_names(),
            "rainfrog",
        );
    }

    fn acorn_io_runtime_acorn_names() -> Vec<&'static str> {
        vec![
            "acorn-v0.10.1-linux-amd64.tar.gz",
            "acorn-v0.10.1-linux-arm64.tar.gz",
            "acorn-v0.10.1-macOS-universal.tar.gz",
            "acorn-v0.10.1-macOS-universal.zip",
            "acorn-v0.10.1-windows-amd64.zip",
            "checksums.txt",
            "checksums.txt.sig",
            "cosign.pub",
        ]
    }

    #[test]
    fn test_acorn_io_runtime_acorn_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 4),
            ],
            &acorn_io_runtime_acorn_names(),
            "acorn",
        );
    }

    fn aduros_wasm4_w4_names() -> Vec<&'static str> {
        vec![
            "w4-linux.zip",
            "w4-mac.zip",
            "w4-windows.zip",
        ]
    }

    #[test]
    fn test_aduros_wasm4_w4_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
                (Platform::Win32, 2),
                (Platform::Win64, 2),
                (Platform::WinArm64, 2),
            ],
            &aduros_wasm4_w4_names(),
            "w4",
        );
    }

    fn afnanenayet_diffsitter_diffsitter_names() -> Vec<&'static str> {
        vec![
            "diffsitter-aarch64-apple-darwin.sha256",
            "diffsitter-aarch64-apple-darwin.tar.gz",
            "diffsitter-aarch64-pc-windows-msvc.sha256",
            "diffsitter-aarch64-pc-windows-msvc.zip",
            "diffsitter-aarch64-unknown-linux-gnu.sha256",
            "diffsitter-aarch64-unknown-linux-gnu.tar.gz",
            "diffsitter-arm-unknown-linux-gnueabi.sha256",
            "diffsitter-arm-unknown-linux-gnueabi.tar.gz",
            "diffsitter-i686-unknown-linux-gnu.sha256",
            "diffsitter-i686-unknown-linux-gnu.tar.gz",
            "diffsitter-powerpc64le-unknown-linux-gnu.sha256",
            "diffsitter-powerpc64le-unknown-linux-gnu.tar.gz",
            "diffsitter-riscv64gc-unknown-linux-gnu.sha256",
            "diffsitter-riscv64gc-unknown-linux-gnu.tar.gz",
            "diffsitter-x86_64-apple-darwin.sha256",
            "diffsitter-x86_64-apple-darwin.tar.gz",
            "diffsitter-x86_64-pc-windows-msvc.sha256",
            "diffsitter-x86_64-pc-windows-msvc.zip",
            "diffsitter-x86_64-unknown-freebsd.sha256",
            "diffsitter-x86_64-unknown-freebsd.tar.gz",
            "diffsitter-x86_64-unknown-linux-gnu.sha256",
            "diffsitter-x86_64-unknown-linux-gnu.tar.gz",
            "diffsitter_src.tar.gz",
            "diffsitter_v0.9.0_amd64.deb",
        ]
    }

    #[test]
    fn test_afnanenayet_diffsitter_diffsitter_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 21),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 15),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 17),
                (Platform::WinArm64, 3),
            ],
            &afnanenayet_diffsitter_diffsitter_names(),
            "diffsitter",
        );
    }

    fn akiomik_mado_mado_names() -> Vec<&'static str> {
        vec![
            "mado-Linux-gnu-arm64.tar.gz",
            "mado-Linux-gnu-arm64.tar.gz.sha256",
            "mado-Linux-gnu-x86_64.tar.gz",
            "mado-Linux-gnu-x86_64.tar.gz.sha256",
            "mado-macOS-arm64.tar.gz",
            "mado-macOS-arm64.tar.gz.sha256",
            "mado-macOS-x86_64.tar.gz",
            "mado-macOS-x86_64.tar.gz.sha256",
            "mado-Windows-msvc-x86_64.zip",
            "mado-Windows-msvc-x86_64.zip.sha256",
        ]
    }

    #[test]
    fn test_akiomik_mado_mado_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 8),
            ],
            &akiomik_mado_mado_names(),
            "mado",
        );
    }

    fn alexpasmantier_television_tv_names() -> Vec<&'static str> {
        vec![
            "tv-0.15.3-aarch64-apple-darwin.sha256",
            "tv-0.15.3-aarch64-apple-darwin.tar.gz",
            "tv-0.15.3-aarch64-unknown-linux-gnu.deb",
            "tv-0.15.3-aarch64-unknown-linux-gnu.deb.sha256",
            "tv-0.15.3-aarch64-unknown-linux-gnu.sha256",
            "tv-0.15.3-aarch64-unknown-linux-gnu.tar.gz",
            "tv-0.15.3-i686-unknown-linux-gnu.sha256",
            "tv-0.15.3-i686-unknown-linux-gnu.tar.gz",
            "tv-0.15.3-x86_64-apple-darwin.sha256",
            "tv-0.15.3-x86_64-apple-darwin.tar.gz",
            "tv-0.15.3-x86_64-pc-windows-msvc.sha256",
            "tv-0.15.3-x86_64-pc-windows-msvc.tar.gz",
            "tv-0.15.3-x86_64-pc-windows-msvc.zip",
            "tv-0.15.3-x86_64-pc-windows-msvc.zip.sha256",
            "tv-0.15.3-x86_64-unknown-linux-gnu.deb",
            "tv-0.15.3-x86_64-unknown-linux-gnu.deb.sha256",
            "tv-0.15.3-x86_64-unknown-linux-gnu.sha256",
            "tv-0.15.3-x86_64-unknown-linux-gnu.tar.gz",
            "tv-0.15.3-x86_64-unknown-linux-musl.deb",
            "tv-0.15.3-x86_64-unknown-linux-musl.deb.sha256",
            "tv-0.15.3-x86_64-unknown-linux-musl.sha256",
            "tv-0.15.3-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_alexpasmantier_television_tv_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 21),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 11),
            ],
            &alexpasmantier_television_tv_names(),
            "tv",
        );
    }

    fn allero_io_allero_allero_names() -> Vec<&'static str> {
        vec![
            "allero_0.0.27_Darwin_arm64.zip",
            "allero_0.0.27_Darwin_x86_64.zip",
            "allero_0.0.27_Linux_386.zip",
            "allero_0.0.27_Linux_arm64.zip",
            "allero_0.0.27_Linux_x86_64.zip",
            "allero_0.0.27_windows_386.zip",
            "allero_0.0.27_windows_arm64.zip",
            "allero_0.0.27_windows_x86_64.zip",
            "checksums.txt",
        ]
    }

    #[test]
    fn test_allero_io_allero_allero_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 2),
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win32, 5),
                (Platform::Win64, 7),
                (Platform::WinArm64, 6),
            ],
            &allero_io_allero_allero_names(),
            "allero",
        );
    }

    fn altsem_gitu_gitu_names() -> Vec<&'static str> {
        vec![
            "gitu-v0.40.0-aarch64-apple-darwin.zip",
            "gitu-v0.40.0-x86_64-apple-darwin.zip",
            "gitu-v0.40.0-x86_64-pc-windows-msvc.zip",
            "gitu-v0.40.0-x86_64-unknown-linux-gnu.zip",
            "gitu-v0.40.0-x86_64-unknown-linux-musl.zip",
        ]
    }

    #[test]
    fn test_altsem_gitu_gitu_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 2),
            ],
            &altsem_gitu_gitu_names(),
            "gitu",
        );
    }

    fn alvinunreal_tmuxai_tmuxai_names() -> Vec<&'static str> {
        vec![
            "checksums.sha256",
            "tmuxai_Darwin_amd64.tar.gz",
            "tmuxai_Darwin_arm64.tar.gz",
            "tmuxai_Freebsd_amd64.tar.gz",
            "tmuxai_Freebsd_arm64.tar.gz",
            "tmuxai_Freebsd_armv7.tar.gz",
            "tmuxai_linux_amd64.apk",
            "tmuxai_linux_amd64.deb",
            "tmuxai_linux_amd64.rpm",
            "tmuxai_Linux_amd64.tar.gz",
            "tmuxai_linux_arm.apk",
            "tmuxai_linux_arm.deb",
            "tmuxai_linux_arm.rpm",
            "tmuxai_linux_arm64.apk",
            "tmuxai_linux_arm64.deb",
            "tmuxai_linux_arm64.rpm",
            "tmuxai_Linux_arm64.tar.gz",
            "tmuxai_Linux_armv7.tar.gz",
            "tmuxai_linux_ppc64le.apk",
            "tmuxai_linux_ppc64le.deb",
            "tmuxai_linux_ppc64le.rpm",
            "tmuxai_Linux_ppc64le.tar.gz",
            "tmuxai_linux_s390x.apk",
            "tmuxai_linux_s390x.deb",
            "tmuxai_linux_s390x.rpm",
            "tmuxai_Linux_s390x.tar.gz",
        ]
    }

    #[test]
    fn test_alvinunreal_tmuxai_tmuxai_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 16),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
            ],
            &alvinunreal_tmuxai_tmuxai_names(),
            "tmuxai",
        );
    }

    fn amacneil_dbmate_dbmate_names() -> Vec<&'static str> {
        vec![
            "dbmate-linux-386",
            "dbmate-linux-amd64",
            "dbmate-linux-arm",
            "dbmate-linux-arm64",
            "dbmate-macos-amd64",
            "dbmate-macos-arm64",
            "dbmate-windows-amd64.exe",
        ]
    }

    #[test]
    fn test_amacneil_dbmate_dbmate_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 0),
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 5),
            ],
            &amacneil_dbmate_dbmate_names(),
            "dbmate",
        );
    }

    fn amalshaji_portr_portr_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "portr_0.0.45-beta_Darwin_arm64.zip",
            "portr_0.0.45-beta_Darwin_x86_64.zip",
            "portr_0.0.45-beta_Linux_arm64.zip",
            "portr_0.0.45-beta_Linux_x86_64.zip",
            "portr_0.0.45-beta_Windows_arm64.zip",
            "portr_0.0.45-beta_Windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_amalshaji_portr_portr_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 6),
                (Platform::WinArm64, 5),
            ],
            &amalshaji_portr_portr_names(),
            "portr",
        );
    }

    fn amir20_dtop_dtop_names() -> Vec<&'static str> {
        vec![
            "dist-manifest.json",
            "dtop-aarch64-apple-darwin.tar.gz",
            "dtop-aarch64-apple-darwin.tar.gz.sha256",
            "dtop-aarch64-unknown-linux-gnu.tar.gz",
            "dtop-aarch64-unknown-linux-gnu.tar.gz.sha256",
            "dtop-installer.sh",
            "dtop-x86_64-apple-darwin.tar.gz",
            "dtop-x86_64-apple-darwin.tar.gz.sha256",
            "dtop-x86_64-unknown-linux-gnu.tar.gz",
            "dtop-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_amir20_dtop_dtop_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 1),
            ],
            &amir20_dtop_dtop_names(),
            "dtop",
        );
    }

    fn ampcode_zvelte_check_zvelte_check_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "zvelte-check-darwin-aarch64.tar.gz",
            "zvelte-check-darwin-x86_64.tar.gz",
            "zvelte-check-linux-aarch64.tar.gz",
            "zvelte-check-linux-x86_64.tar.gz",
            "zvelte-check-windows-x86_64.zip",
        ]
    }

    #[test]
    fn test_ampcode_zvelte_check_zvelte_check_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 5),
            ],
            &ampcode_zvelte_check_zvelte_check_names(),
            "zvelte-check",
        );
    }

    fn anchore_syft_syft_names() -> Vec<&'static str> {
        vec![
            "syft_1.42.1_checksums.txt",
            "syft_1.42.1_checksums.txt.pem",
            "syft_1.42.1_checksums.txt.sig",
            "syft_1.42.1_darwin_amd64.sbom",
            "syft_1.42.1_darwin_amd64.tar.gz",
            "syft_1.42.1_darwin_arm64.sbom",
            "syft_1.42.1_darwin_arm64.tar.gz",
            "syft_1.42.1_linux_amd64.deb",
            "syft_1.42.1_linux_amd64.rpm",
            "syft_1.42.1_linux_amd64.sbom",
            "syft_1.42.1_linux_amd64.tar.gz",
            "syft_1.42.1_linux_arm64.deb",
            "syft_1.42.1_linux_arm64.rpm",
            "syft_1.42.1_linux_arm64.sbom",
            "syft_1.42.1_linux_arm64.tar.gz",
            "syft_1.42.1_linux_ppc64le.deb",
            "syft_1.42.1_linux_ppc64le.rpm",
            "syft_1.42.1_linux_ppc64le.sbom",
            "syft_1.42.1_linux_ppc64le.tar.gz",
            "syft_1.42.1_linux_s390x.deb",
            "syft_1.42.1_linux_s390x.rpm",
            "syft_1.42.1_linux_s390x.sbom",
            "syft_1.42.1_linux_s390x.tar.gz",
            "syft_1.42.1_windows_amd64.sbom",
            "syft_1.42.1_windows_amd64.zip",
            "syft_1.42.1_windows_arm64.sbom",
            "syft_1.42.1_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_anchore_syft_syft_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 14),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 6),
                (Platform::Win64, 24),
                (Platform::WinArm64, 26),
            ],
            &anchore_syft_syft_names(),
            "syft",
        );
    }

    fn andreazorzetto_yh_yh_names() -> Vec<&'static str> {
        vec![
            "yh-linux-386.zip",
            "yh-linux-amd64.zip",
            "yh-osx-amd64.zip",
            "yh-windows-amd64.zip",
        ]
    }

    #[test]
    fn test_andreazorzetto_yh_yh_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 2),
                (Platform::Win64, 3),
            ],
            &andreazorzetto_yh_yh_names(),
            "yh",
        );
    }

    fn ankitpokhrel_jira_cli_jira_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "jira_1.7.0_linux_arm64.tar.gz",
            "jira_1.7.0_linux_armv6.tar.gz",
            "jira_1.7.0_linux_i386.tar.gz",
            "jira_1.7.0_linux_x86_64.tar.gz",
            "jira_1.7.0_macOS_arm64.tar.gz",
            "jira_1.7.0_macOS_x86_64.tar.gz",
            "jira_1.7.0_windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_ankitpokhrel_jira_cli_jira_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 5),
                (Platform::Win64, 7),
            ],
            &ankitpokhrel_jira_cli_jira_names(),
            "jira",
        );
    }

    fn anomalyco_opencode_opencode_names() -> Vec<&'static str> {
        vec![
            "latest.json",
            "opencode-darwin-arm64.zip",
            "opencode-darwin-x64-baseline.zip",
            "opencode-darwin-x64.zip",
            "opencode-desktop-darwin-aarch64.app.tar.gz",
            "opencode-desktop-darwin-aarch64.app.tar.gz.sig",
            "opencode-desktop-darwin-aarch64.dmg",
            "opencode-desktop-darwin-x64.app.tar.gz",
            "opencode-desktop-darwin-x64.app.tar.gz.sig",
            "opencode-desktop-darwin-x64.dmg",
            "opencode-desktop-linux-aarch64.rpm",
            "opencode-desktop-linux-aarch64.rpm.sig",
            "opencode-desktop-linux-amd64.deb",
            "opencode-desktop-linux-amd64.deb.sig",
            "opencode-desktop-linux-arm64.deb",
            "opencode-desktop-linux-arm64.deb.sig",
            "opencode-desktop-linux-x86_64.rpm",
            "opencode-desktop-linux-x86_64.rpm.sig",
            "opencode-desktop-windows-x64.exe",
            "opencode-desktop-windows-x64.exe.sig",
            "opencode-linux-arm64-musl.tar.gz",
            "opencode-linux-arm64.tar.gz",
            "opencode-linux-x64-baseline-musl.tar.gz",
            "opencode-linux-x64-baseline.tar.gz",
            "opencode-linux-x64-musl.tar.gz",
            "opencode-linux-x64.tar.gz",
            "opencode-windows-x64-baseline.zip",
            "opencode-windows-x64.zip",
        ]
    }

    #[test]
    fn test_anomalyco_opencode_opencode_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 25),
                (Platform::LinuxAarch64, 21),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 27),
            ],
            &anomalyco_opencode_opencode_names(),
            "opencode",
        );
    }

    fn apache_maven_mvnd_maven_mvnd_names() -> Vec<&'static str> {
        vec![
            "maven-mvnd-1.0.3-darwin-aarch64.tar.gz",
            "maven-mvnd-1.0.3-darwin-aarch64.zip",
            "maven-mvnd-1.0.3-darwin-amd64.tar.gz",
            "maven-mvnd-1.0.3-darwin-amd64.zip",
            "maven-mvnd-1.0.3-linux-amd64.tar.gz",
            "maven-mvnd-1.0.3-linux-amd64.zip",
            "maven-mvnd-1.0.3-src.tar.gz",
            "maven-mvnd-1.0.3-src.zip",
            "maven-mvnd-1.0.3-windows-amd64.tar.gz",
            "maven-mvnd-1.0.3-windows-amd64.zip",
        ]
    }

    #[test]
    fn test_apache_maven_mvnd_maven_mvnd_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 8),
            ],
            &apache_maven_mvnd_maven_mvnd_names(),
            "maven-mvnd",
        );
    }

    fn aporia_ai_kubesurvival_kubesurvival_names() -> Vec<&'static str> {
        vec![
            "kubesurvival_checksums.txt",
            "KubeSurvival_Darwin_arm64.tar.gz",
            "KubeSurvival_Darwin_x86_64.tar.gz",
            "KubeSurvival_Linux_arm64.tar.gz",
            "KubeSurvival_Linux_i386.tar.gz",
            "KubeSurvival_Linux_x86_64.tar.gz",
            "KubeSurvival_Windows_i386.zip",
            "KubeSurvival_Windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_aporia_ai_kubesurvival_kubesurvival_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 7),
            ],
            &aporia_ai_kubesurvival_kubesurvival_names(),
            "KubeSurvival",
        );
    }

    fn apple_pkl_pkl_names() -> Vec<&'static str> {
        vec![
            "jpkl",
            "jpkldoc",
            "pkl-alpine-linux-amd64",
            "pkl-codegen-java",
            "pkl-codegen-kotlin",
            "pkl-linux-aarch64",
            "pkl-linux-amd64",
            "pkl-macos-aarch64",
            "pkl-macos-amd64",
            "pkl-windows-amd64.exe",
            "pkldoc-alpine-linux-amd64",
            "pkldoc-linux-aarch64",
            "pkldoc-linux-amd64",
            "pkldoc-macos-aarch64",
            "pkldoc-macos-amd64",
            "pkldoc-windows-amd64.exe",
        ]
    }

    #[test]
    fn test_apple_pkl_pkl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 8),
                (Platform::OsxArm64, 7),
            ],
            &apple_pkl_pkl_names(),
            "pkl",
        );
    }

    fn aquasecurity_chain_bench_chain_bench_names() -> Vec<&'static str> {
        vec![
            "chain-bench_0.1.10_Linux-64bit.tar.gz",
            "chain-bench_0.1.10_Linux-ARM64.tar.gz",
            "chain-bench_0.1.10_macOS-64bit.tar.gz",
            "chain-bench_0.1.10_macOS-ARM64.tar.gz",
            "checksums.txt",
        ]
    }

    #[test]
    fn test_aquasecurity_chain_bench_chain_bench_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 3),
            ],
            &aquasecurity_chain_bench_chain_bench_names(),
            "chain-bench",
        );
    }

    fn aquasecurity_starboard_starboard_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "starboard_darwin_ARM64.tar.gz",
            "starboard_darwin_x86_64.tar.gz",
            "starboard_linux_ARM.tar.gz",
            "starboard_linux_ARM64.tar.gz",
            "starboard_linux_x86_64.tar.gz",
            "starboard_windows_ARM.zip",
            "starboard_windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_aquasecurity_starboard_starboard_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 7),
                (Platform::WinArm64, 6),
            ],
            &aquasecurity_starboard_starboard_names(),
            "starboard",
        );
    }

    fn aquasecurity_trivy_trivy_names() -> Vec<&'static str> {
        vec![
            "bom.json",
            "trivy_0.69.3_checksums.txt",
            "trivy_0.69.3_checksums.txt.sigstore.json",
            "trivy_0.69.3_FreeBSD-64bit.tar.gz",
            "trivy_0.69.3_FreeBSD-64bit.tar.gz.sigstore.json",
            "trivy_0.69.3_Linux-32bit.deb",
            "trivy_0.69.3_Linux-32bit.deb.sigstore.json",
            "trivy_0.69.3_Linux-32bit.rpm",
            "trivy_0.69.3_Linux-32bit.rpm.sigstore.json",
            "trivy_0.69.3_Linux-32bit.tar.gz",
            "trivy_0.69.3_Linux-32bit.tar.gz.sigstore.json",
            "trivy_0.69.3_Linux-64bit.deb",
            "trivy_0.69.3_Linux-64bit.deb.sigstore.json",
            "trivy_0.69.3_Linux-64bit.rpm",
            "trivy_0.69.3_Linux-64bit.rpm.sigstore.json",
            "trivy_0.69.3_Linux-64bit.tar.gz",
            "trivy_0.69.3_Linux-64bit.tar.gz.sigstore.json",
            "trivy_0.69.3_Linux-ARM.deb",
            "trivy_0.69.3_Linux-ARM.deb.sigstore.json",
            "trivy_0.69.3_Linux-ARM.rpm",
            "trivy_0.69.3_Linux-ARM.rpm.sigstore.json",
            "trivy_0.69.3_Linux-ARM.tar.gz",
            "trivy_0.69.3_Linux-ARM.tar.gz.sigstore.json",
            "trivy_0.69.3_Linux-ARM64.deb",
            "trivy_0.69.3_Linux-ARM64.deb.sigstore.json",
            "trivy_0.69.3_Linux-ARM64.rpm",
            "trivy_0.69.3_Linux-ARM64.rpm.sigstore.json",
            "trivy_0.69.3_Linux-ARM64.tar.gz",
            "trivy_0.69.3_Linux-ARM64.tar.gz.sigstore.json",
            "trivy_0.69.3_Linux-PPC64LE.deb",
            "trivy_0.69.3_Linux-PPC64LE.deb.sigstore.json",
            "trivy_0.69.3_Linux-PPC64LE.rpm",
            "trivy_0.69.3_Linux-PPC64LE.rpm.sigstore.json",
            "trivy_0.69.3_Linux-PPC64LE.tar.gz",
            "trivy_0.69.3_Linux-PPC64LE.tar.gz.sigstore.json",
            "trivy_0.69.3_Linux-s390x.deb",
            "trivy_0.69.3_Linux-s390x.deb.sigstore.json",
            "trivy_0.69.3_Linux-s390x.rpm",
            "trivy_0.69.3_Linux-s390x.rpm.sigstore.json",
            "trivy_0.69.3_Linux-s390x.tar.gz",
            "trivy_0.69.3_Linux-s390x.tar.gz.sigstore.json",
            "trivy_0.69.3_macOS-64bit.tar.gz",
            "trivy_0.69.3_macOS-64bit.tar.gz.sigstore.json",
            "trivy_0.69.3_macOS-ARM64.tar.gz",
            "trivy_0.69.3_macOS-ARM64.tar.gz.sigstore.json",
            "trivy_0.69.3_windows-64bit.zip",
            "trivy_0.69.3_windows-64bit.zip.sigstore.json",
        ]
    }

    #[test]
    fn test_aquasecurity_trivy_trivy_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 15),
                (Platform::LinuxAarch64, 27),
                (Platform::Osx64, 41),
                (Platform::OsxArm64, 43),
                (Platform::Win64, 45),
            ],
            &aquasecurity_trivy_trivy_names(),
            "trivy",
        );
    }

    fn arduino_arduino_cli_arduino_cli_names() -> Vec<&'static str> {
        vec![
            "1.4.1-checksums.txt",
            "arduino-cli_1.4.1-1_amd64.deb",
            "arduino-cli_1.4.1-1_arm64.deb",
            "arduino-cli_1.4.1-1_armel.deb",
            "arduino-cli_1.4.1-1_armhf.deb",
            "arduino-cli_1.4.1-1_i386.deb",
            "arduino-cli_1.4.1_configuration.schema.json",
            "arduino-cli_1.4.1_Linux_32bit.tar.gz",
            "arduino-cli_1.4.1_Linux_64bit.tar.gz",
            "arduino-cli_1.4.1_Linux_ARM64.tar.gz",
            "arduino-cli_1.4.1_Linux_ARMv6.tar.gz",
            "arduino-cli_1.4.1_Linux_ARMv7.tar.gz",
            "arduino-cli_1.4.1_macOS_64bit.tar.gz",
            "arduino-cli_1.4.1_macOS_ARM64.tar.gz",
            "arduino-cli_1.4.1_proto.zip",
            "arduino-cli_1.4.1_Windows_32bit.zip",
            "arduino-cli_1.4.1_Windows_64bit.msi",
            "arduino-cli_1.4.1_Windows_64bit.zip",
            "CHANGELOG.md",
        ]
    }

    #[test]
    fn test_arduino_arduino_cli_arduino_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::LinuxAarch64, 9),
                (Platform::Osx64, 12),
                (Platform::OsxArm64, 13),
                (Platform::Win64, 17),
            ],
            &arduino_arduino_cli_arduino_cli_names(),
            "arduino-cli",
        );
    }

    fn argoproj_labs_argocd_image_updater_argocd_image_updater_names() -> Vec<&'static str> {
        vec![
            "argocd-image-updater-darwin_amd64",
            "argocd-image-updater-darwin_arm64",
            "argocd-image-updater-linux_amd64",
            "argocd-image-updater-linux_arm64",
            "argocd-image-updater-win64.exe",
            "release-v1.1.1.sha256",
            "release-v1.1.1.sha256.asc",
        ]
    }

    #[test]
    fn test_argoproj_labs_argocd_image_updater_argocd_image_updater_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 1),
            ],
            &argoproj_labs_argocd_image_updater_argocd_image_updater_names(),
            "argocd-image-updater",
        );
    }

    fn aristocratos_btop_btop_names() -> Vec<&'static str> {
        vec![
            "btop-aarch64-unknown-linux-musl.tbz",
            "btop-arm-unknown-linux-musleabi.tbz",
            "btop-armv7-unknown-linux-musleabi.tbz",
            "btop-i586-unknown-linux-musl.tbz",
            "btop-i686-unknown-linux-musl.tbz",
            "btop-m68k-unknown-linux-musl.tbz",
            "btop-mips64-unknown-linux-musl.tbz",
            "btop-powerpc64-unknown-linux-musl.tbz",
            "btop-riscv64-unknown-linux-musl.tbz",
            "btop-s390x-ibm-linux-musl.tbz",
            "btop-x86_64-unknown-linux-musl.tbz",
        ]
    }

    #[test]
    fn test_aristocratos_btop_btop_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 0),
            ],
            &aristocratos_btop_btop_names(),
            "btop",
        );
    }

    fn arl_gitmux_gitmux_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "gitmux_v0.11.5_linux_386.tar.gz",
            "gitmux_v0.11.5_linux_amd64.tar.gz",
            "gitmux_v0.11.5_linux_arm64.tar.gz",
            "gitmux_v0.11.5_macOS_amd64.tar.gz",
            "gitmux_v0.11.5_macOS_arm64.tar.gz",
        ]
    }

    #[test]
    fn test_arl_gitmux_gitmux_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 1),
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 5),
            ],
            &arl_gitmux_gitmux_names(),
            "gitmux",
        );
    }

    fn artempyanykh_marksman_marksman_names() -> Vec<&'static str> {
        vec![
            "marksman-linux-arm64",
            "marksman-linux-musl-arm64",
            "marksman-linux-musl-x64",
            "marksman-linux-x64",
            "marksman-macos",
            "marksman.exe",
        ]
    }

    #[test]
    fn test_artempyanykh_marksman_marksman_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 4),
            ],
            &artempyanykh_marksman_marksman_names(),
            "marksman",
        );
    }

    fn arxanas_git_branchless_git_branchless_names() -> Vec<&'static str> {
        vec![
            "git-branchless-v0.10.0-x86_64-pc-windows-msvc.zip",
            "git-branchless-v0.10.0-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_arxanas_git_branchless_git_branchless_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Win64, 0),
            ],
            &arxanas_git_branchless_git_branchless_names(),
            "git-branchless",
        );
    }

    fn asciidoctor_asciidoctor_reveal_js_asciidoctor_revealjs_names() -> Vec<&'static str> {
        vec![
            "asciidoctor-revealjs-linux",
            "asciidoctor-revealjs-macos",
            "asciidoctor-revealjs-win.exe",
        ]
    }

    #[test]
    fn test_asciidoctor_asciidoctor_reveal_js_asciidoctor_revealjs_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
            ],
            &asciidoctor_asciidoctor_reveal_js_asciidoctor_revealjs_names(),
            "asciidoctor-revealjs",
        );
    }

    fn assetnote_surf_surf_names() -> Vec<&'static str> {
        vec![
            "surf_0.0.5_checksums.txt",
            "surf_0.0.5_linux_386.tar.gz",
            "surf_0.0.5_linux_amd64.tar.gz",
            "surf_0.0.5_macOS_amd64.tar.gz",
            "surf_0.0.5_windows_386.zip",
            "surf_0.0.5_windows_amd64.zip",
        ]
    }

    #[test]
    fn test_assetnote_surf_surf_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 3),
                (Platform::Win32, 4),
                (Platform::Win64, 5),
            ],
            &assetnote_surf_surf_names(),
            "surf",
        );
    }

    fn ast_grep_ast_grep_app_names() -> Vec<&'static str> {
        vec![
            "app-aarch64-apple-darwin.zip",
            "app-aarch64-pc-windows-msvc.zip",
            "app-aarch64-unknown-linux-gnu.zip",
            "app-i686-pc-windows-msvc.zip",
            "app-x86_64-apple-darwin.zip",
            "app-x86_64-pc-windows-msvc.zip",
            "app-x86_64-unknown-linux-gnu.zip",
        ]
    }

    #[test]
    fn test_ast_grep_ast_grep_app_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 5),
                (Platform::WinArm64, 1),
            ],
            &ast_grep_ast_grep_app_names(),
            "app",
        );
    }

    fn astral_sh_ruff_ruff_names() -> Vec<&'static str> {
        vec![
            "dist-manifest.json",
            "ruff-aarch64-apple-darwin.tar.gz",
            "ruff-aarch64-apple-darwin.tar.gz.sha256",
            "ruff-aarch64-pc-windows-msvc.zip",
            "ruff-aarch64-pc-windows-msvc.zip.sha256",
            "ruff-aarch64-unknown-linux-gnu.tar.gz",
            "ruff-aarch64-unknown-linux-gnu.tar.gz.sha256",
            "ruff-aarch64-unknown-linux-musl.tar.gz",
            "ruff-aarch64-unknown-linux-musl.tar.gz.sha256",
            "ruff-arm-unknown-linux-musleabihf.tar.gz",
            "ruff-arm-unknown-linux-musleabihf.tar.gz.sha256",
            "ruff-armv7-unknown-linux-gnueabihf.tar.gz",
            "ruff-armv7-unknown-linux-gnueabihf.tar.gz.sha256",
            "ruff-armv7-unknown-linux-musleabihf.tar.gz",
            "ruff-armv7-unknown-linux-musleabihf.tar.gz.sha256",
            "ruff-i686-pc-windows-msvc.zip",
            "ruff-i686-pc-windows-msvc.zip.sha256",
            "ruff-i686-unknown-linux-gnu.tar.gz",
            "ruff-i686-unknown-linux-gnu.tar.gz.sha256",
            "ruff-i686-unknown-linux-musl.tar.gz",
            "ruff-i686-unknown-linux-musl.tar.gz.sha256",
            "ruff-installer.ps1",
            "ruff-installer.sh",
            "ruff-powerpc64le-unknown-linux-gnu.tar.gz",
            "ruff-powerpc64le-unknown-linux-gnu.tar.gz.sha256",
            "ruff-riscv64gc-unknown-linux-gnu.tar.gz",
            "ruff-riscv64gc-unknown-linux-gnu.tar.gz.sha256",
            "ruff-s390x-unknown-linux-gnu.tar.gz",
            "ruff-s390x-unknown-linux-gnu.tar.gz.sha256",
            "ruff-x86_64-apple-darwin.tar.gz",
            "ruff-x86_64-apple-darwin.tar.gz.sha256",
            "ruff-x86_64-pc-windows-msvc.zip",
            "ruff-x86_64-pc-windows-msvc.zip.sha256",
            "ruff-x86_64-unknown-linux-gnu.tar.gz",
            "ruff-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "ruff-x86_64-unknown-linux-musl.tar.gz",
            "ruff-x86_64-unknown-linux-musl.tar.gz.sha256",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_astral_sh_ruff_ruff_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 35),
                (Platform::LinuxAarch64, 7),
                (Platform::Osx64, 29),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 31),
                (Platform::WinArm64, 3),
            ],
            &astral_sh_ruff_ruff_names(),
            "ruff",
        );
    }

    fn astral_sh_rye_rye_names() -> Vec<&'static str> {
        vec![
            "MANIFEST.json",
            "rye-aarch64-linux.gz",
            "rye-aarch64-linux.gz.sha256",
            "rye-aarch64-macos.gz",
            "rye-aarch64-macos.gz.sha256",
            "rye-x86-windows.exe",
            "rye-x86-windows.exe.sha256",
            "rye-x86_64-linux.gz",
            "rye-x86_64-linux.gz.sha256",
            "rye-x86_64-macos.gz",
            "rye-x86_64-macos.gz.sha256",
            "rye-x86_64-windows.exe",
            "rye-x86_64-windows.exe.sha256",
        ]
    }

    #[test]
    fn test_astral_sh_rye_rye_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 3),
            ],
            &astral_sh_rye_rye_names(),
            "rye",
        );
    }

    fn atuinsh_atuin_atuin_names() -> Vec<&'static str> {
        vec![
            "atuin-aarch64-apple-darwin-update",
            "atuin-aarch64-apple-darwin.tar.gz",
            "atuin-aarch64-apple-darwin.tar.gz.sha256",
            "atuin-aarch64-unknown-linux-gnu-update",
            "atuin-aarch64-unknown-linux-gnu.tar.gz",
            "atuin-aarch64-unknown-linux-gnu.tar.gz.sha256",
            "atuin-aarch64-unknown-linux-musl-update",
            "atuin-aarch64-unknown-linux-musl.tar.gz",
            "atuin-aarch64-unknown-linux-musl.tar.gz.sha256",
            "atuin-installer.ps1",
            "atuin-installer.sh",
            "atuin-server-aarch64-apple-darwin-update",
            "atuin-server-aarch64-apple-darwin.tar.gz",
            "atuin-server-aarch64-apple-darwin.tar.gz.sha256",
            "atuin-server-aarch64-unknown-linux-gnu-update",
            "atuin-server-aarch64-unknown-linux-gnu.tar.gz",
            "atuin-server-aarch64-unknown-linux-gnu.tar.gz.sha256",
            "atuin-server-aarch64-unknown-linux-musl-update",
            "atuin-server-aarch64-unknown-linux-musl.tar.gz",
            "atuin-server-aarch64-unknown-linux-musl.tar.gz.sha256",
            "atuin-server-installer.ps1",
            "atuin-server-installer.sh",
            "atuin-server-x86_64-pc-windows-msvc-update",
            "atuin-server-x86_64-pc-windows-msvc.zip",
            "atuin-server-x86_64-pc-windows-msvc.zip.sha256",
            "atuin-server-x86_64-unknown-linux-gnu-update",
            "atuin-server-x86_64-unknown-linux-gnu.tar.gz",
            "atuin-server-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "atuin-server-x86_64-unknown-linux-musl-update",
            "atuin-server-x86_64-unknown-linux-musl.tar.gz",
            "atuin-server-x86_64-unknown-linux-musl.tar.gz.sha256",
            "atuin-x86_64-pc-windows-msvc-update",
            "atuin-x86_64-pc-windows-msvc.zip",
            "atuin-x86_64-pc-windows-msvc.zip.sha256",
            "atuin-x86_64-unknown-linux-gnu-update",
            "atuin-x86_64-unknown-linux-gnu.tar.gz",
            "atuin-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "atuin-x86_64-unknown-linux-musl-update",
            "atuin-x86_64-unknown-linux-musl.tar.gz",
            "atuin-x86_64-unknown-linux-musl.tar.gz.sha256",
            "dist-manifest.json",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_atuinsh_atuin_atuin_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 38),
                (Platform::LinuxAarch64, 7),
                (Platform::OsxArm64, 1),
            ],
            &atuinsh_atuin_atuin_names(),
            "atuin",
        );
    }

    fn autobrr_mkbrr_mkbrr_names() -> Vec<&'static str> {
        vec![
            "mkbrr_1.20.0_checksums.txt",
            "mkbrr_1.20.0_darwin_arm64.tar.gz",
            "mkbrr_1.20.0_darwin_x86_64.tar.gz",
            "mkbrr_1.20.0_freebsd_x86_64.tar.gz",
            "mkbrr_1.20.0_linux_amd64.apk",
            "mkbrr_1.20.0_linux_amd64.deb",
            "mkbrr_1.20.0_linux_amd64.pkg.tar.zst",
            "mkbrr_1.20.0_linux_amd64.rpm",
            "mkbrr_1.20.0_linux_arm.tar.gz",
            "mkbrr_1.20.0_linux_arm64.apk",
            "mkbrr_1.20.0_linux_arm64.deb",
            "mkbrr_1.20.0_linux_arm64.pkg.tar.zst",
            "mkbrr_1.20.0_linux_arm64.rpm",
            "mkbrr_1.20.0_linux_arm64.tar.gz",
            "mkbrr_1.20.0_linux_armv6.apk",
            "mkbrr_1.20.0_linux_armv6.deb",
            "mkbrr_1.20.0_linux_armv6.rpm",
            "mkbrr_1.20.0_linux_x86_64.tar.gz",
            "mkbrr_1.20.0_windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_autobrr_mkbrr_mkbrr_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 17),
                (Platform::LinuxAarch64, 13),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 18),
            ],
            &autobrr_mkbrr_mkbrr_names(),
            "mkbrr",
        );
    }

    fn aws_cloudformation_rain_rain_names() -> Vec<&'static str> {
        vec![
            "rain-v1.24.3_darwin-amd64-nocgo.zip",
            "rain-v1.24.3_darwin-amd64.zip",
            "rain-v1.24.3_darwin-arm64-nocgo.zip",
            "rain-v1.24.3_darwin-arm64.zip",
            "rain-v1.24.3_linux-amd64-nocgo.zip",
            "rain-v1.24.3_linux-amd64.zip",
            "rain-v1.24.3_linux-arm-nocgo.zip",
            "rain-v1.24.3_linux-arm.zip",
            "rain-v1.24.3_linux-arm64-nocgo.zip",
            "rain-v1.24.3_linux-arm64.zip",
            "rain-v1.24.3_linux-i386-nocgo.zip",
            "rain-v1.24.3_linux-i386.zip",
            "rain-v1.24.3_windows-amd64-nocgo.zip",
            "rain-v1.24.3_windows-amd64.zip",
            "rain-v1.24.3_windows-i386-nocgo.zip",
            "rain-v1.24.3_windows-i386.zip",
        ]
    }

    #[test]
    fn test_aws_cloudformation_rain_rain_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 13),
            ],
            &aws_cloudformation_rain_rain_names(),
            "rain",
        );
    }

    fn aws_aws_sam_cli_aws_sam_cli_names() -> Vec<&'static str> {
        vec![
            "aws-sam-cli-linux-arm64.zip",
            "aws-sam-cli-linux-arm64.zip.sig",
            "aws-sam-cli-linux-x86_64.zip",
            "aws-sam-cli-linux-x86_64.zip.sig",
            "aws-sam-cli-macos-arm64.pkg",
            "aws-sam-cli-macos-x86_64.pkg",
            "AWS_SAM_CLI_64_PY3.msi",
        ]
    }

    #[test]
    fn test_aws_aws_sam_cli_aws_sam_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 4),
            ],
            &aws_aws_sam_cli_aws_sam_cli_names(),
            "aws-sam-cli",
        );
    }

    fn aws_copilot_cli_copilot_names() -> Vec<&'static str> {
        vec![
            "copilot-darwin",
            "copilot-darwin-amd64",
            "copilot-darwin-amd64-v1.34.1",
            "copilot-darwin-amd64-v1.34.1.asc",
            "copilot-darwin-amd64-v1.34.1.md5",
            "copilot-darwin-amd64.asc",
            "copilot-darwin-amd64.md5",
            "copilot-darwin-arm64",
            "copilot-darwin-arm64-v1.34.1",
            "copilot-darwin-arm64-v1.34.1.asc",
            "copilot-darwin-arm64-v1.34.1.md5",
            "copilot-darwin-arm64.asc",
            "copilot-darwin-arm64.md5",
            "copilot-darwin-v1.34.1",
            "copilot-darwin-v1.34.1.asc",
            "copilot-darwin-v1.34.1.md5",
            "copilot-darwin.asc",
            "copilot-darwin.md5",
            "copilot-linux",
            "copilot-linux-amd64-v1.34.1",
            "copilot-linux-amd64-v1.34.1.asc",
            "copilot-linux-amd64-v1.34.1.md5",
            "copilot-linux-arm64",
            "copilot-linux-arm64-v1.34.1",
            "copilot-linux-arm64-v1.34.1.asc",
            "copilot-linux-arm64-v1.34.1.md5",
            "copilot-linux-arm64.asc",
            "copilot-linux-arm64.md5",
            "copilot-linux-v1.34.1",
            "copilot-linux-v1.34.1.asc",
            "copilot-linux-v1.34.1.md5",
            "copilot-linux.asc",
            "copilot-linux.md5",
            "copilot-windows-v1.34.1.exe",
            "copilot-windows-v1.34.1.exe.asc",
            "copilot-windows-v1.34.1.exe.md5",
            "copilot-windows.exe",
            "copilot-windows.exe.asc",
            "copilot-windows.exe.md5",
            "copilot_1.34.1_linux_amd64.tar.gz",
            "copilot_1.34.1_linux_arm64.tar.gz",
            "copilot_1.34.1_macOS_amd64.tar.gz",
            "copilot_1.34.1_macOS_arm64.tar.gz",
        ]
    }

    #[test]
    fn test_aws_copilot_cli_copilot_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 39),
                (Platform::LinuxAarch64, 40),
                (Platform::Osx64, 41),
                (Platform::OsxArm64, 42),
                (Platform::Win64, 36),
            ],
            &aws_copilot_cli_copilot_names(),
            "copilot",
        );
    }

    fn awslabs_dynein_dynein_names() -> Vec<&'static str> {
        vec![
            "dynein-linux-arm.tar.gz",
            "dynein-linux-arm.tar.gz.sha256",
            "dynein-linux.tar.gz",
            "dynein-linux.tar.gz.sha256",
            "dynein-macos-arm.tar.gz",
            "dynein-macos-arm.tar.gz.sha256",
            "dynein-macos.tar.gz",
            "dynein-macos.tar.gz.sha256",
            "dynein-windows.zip",
            "dynein-windows.zip.sha256",
        ]
    }

    #[test]
    fn test_awslabs_dynein_dynein_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 2),
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 6),
                (Platform::Win32, 8),
                (Platform::Win64, 8),
                (Platform::WinArm64, 8),
            ],
            &awslabs_dynein_dynein_names(),
            "dynein",
        );
    }

    fn awslabs_eks_auto_mode_ebs_migration_tool_eks_auto_mode_ebs_migration_tool_names() -> Vec<&'static str> {
        vec![
            "eks-auto-mode-ebs-migration-tool_0.5.2_sha256_checksums.txt",
            "eks-auto-mode-ebs-migration-tool_Darwin_all",
            "eks-auto-mode-ebs-migration-tool_Linux_arm64",
            "eks-auto-mode-ebs-migration-tool_Linux_x86_64",
        ]
    }

    #[test]
    fn test_awslabs_eks_auto_mode_ebs_migration_tool_eks_auto_mode_ebs_migration_tool_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
            ],
            &awslabs_eks_auto_mode_ebs_migration_tool_eks_auto_mode_ebs_migration_tool_names(),
            "eks-auto-mode-ebs-migration-tool",
        );
    }

    fn axodotdev_cargo_dist_cargo_dist_names() -> Vec<&'static str> {
        vec![
            "cargo-dist-aarch64-apple-darwin.tar.xz",
            "cargo-dist-aarch64-apple-darwin.tar.xz.sha256",
            "cargo-dist-aarch64-unknown-linux-gnu.tar.xz",
            "cargo-dist-aarch64-unknown-linux-gnu.tar.xz.sha256",
            "cargo-dist-aarch64-unknown-linux-musl.tar.xz",
            "cargo-dist-aarch64-unknown-linux-musl.tar.xz.sha256",
            "cargo-dist-installer.ps1",
            "cargo-dist-installer.sh",
            "cargo-dist-npm-package.tar.gz",
            "cargo-dist-x86_64-apple-darwin.tar.xz",
            "cargo-dist-x86_64-apple-darwin.tar.xz.sha256",
            "cargo-dist-x86_64-pc-windows-msvc.zip",
            "cargo-dist-x86_64-pc-windows-msvc.zip.sha256",
            "cargo-dist-x86_64-unknown-linux-gnu.tar.xz",
            "cargo-dist-x86_64-unknown-linux-gnu.tar.xz.sha256",
            "cargo-dist-x86_64-unknown-linux-musl.tar.xz",
            "cargo-dist-x86_64-unknown-linux-musl.tar.xz.sha256",
            "cargo-dist.rb",
            "dist-manifest-schema.json",
            "dist-manifest.json",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_axodotdev_cargo_dist_cargo_dist_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 15),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 11),
            ],
            &axodotdev_cargo_dist_cargo_dist_names(),
            "cargo-dist",
        );
    }

    fn babarot_gist_gist_names() -> Vec<&'static str> {
        vec![
            "gist_1.2.6_checksums.txt",
            "gist_darwin_arm64.tar.gz",
            "gist_darwin_x86_64.tar.gz",
            "gist_linux_arm64.tar.gz",
            "gist_linux_i386.tar.gz",
            "gist_linux_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_babarot_gist_gist_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
            ],
            &babarot_gist_gist_names(),
            "gist",
        );
    }

    fn babarot_git_bump_git_bump_names() -> Vec<&'static str> {
        vec![
            "git-bump_0.1.1_checksums.txt",
            "git-bump_darwin_i386.tar.gz",
            "git-bump_darwin_x86_64.tar.gz",
            "git-bump_linux_i386.tar.gz",
            "git-bump_linux_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_babarot_git_bump_git_bump_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::Osx64, 2),
            ],
            &babarot_git_bump_git_bump_names(),
            "git-bump",
        );
    }

    fn bahdotsh_wrkflw_wrkflw_names() -> Vec<&'static str> {
        vec![
            "wrkflw-v0.7.3-linux-x86_64.tar.gz",
            "wrkflw-v0.7.3-macos-arm64.tar.gz",
            "wrkflw-v0.7.3-macos-x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_bahdotsh_wrkflw_wrkflw_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
            ],
            &bahdotsh_wrkflw_wrkflw_names(),
            "wrkflw",
        );
    }

    fn barnybug_cli53_cli53_names() -> Vec<&'static str> {
        vec![
            "cli53-linux-amd64",
            "cli53-linux-arm",
            "cli53-linux-arm64",
            "cli53-mac-amd64",
            "cli53-mac-arm64",
            "cli53-windows-amd64.exe",
            "cli53-windows-arm.exe",
            "cli53-windows-arm64.exe",
            "cli53_0.8.25_checksums.txt",
        ]
    }

    #[test]
    fn test_barnybug_cli53_cli53_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 4),
            ],
            &barnybug_cli53_cli53_names(),
            "cli53",
        );
    }

    fn bazelbuild_bazel_bazel_names() -> Vec<&'static str> {
        vec![
            "bazel-9.0.0-darwin-arm64",
            "bazel-9.0.0-darwin-arm64.sha256",
            "bazel-9.0.0-darwin-arm64.sig",
            "bazel-9.0.0-darwin-x86_64",
            "bazel-9.0.0-darwin-x86_64.sha256",
            "bazel-9.0.0-darwin-x86_64.sig",
            "bazel-9.0.0-dist.zip",
            "bazel-9.0.0-dist.zip.sha256",
            "bazel-9.0.0-dist.zip.sig",
            "bazel-9.0.0-installer-darwin-arm64.sh",
            "bazel-9.0.0-installer-darwin-arm64.sh.sha256",
            "bazel-9.0.0-installer-darwin-arm64.sh.sig",
            "bazel-9.0.0-installer-darwin-x86_64.sh",
            "bazel-9.0.0-installer-darwin-x86_64.sh.sha256",
            "bazel-9.0.0-installer-darwin-x86_64.sh.sig",
            "bazel-9.0.0-installer-linux-x86_64.sh",
            "bazel-9.0.0-installer-linux-x86_64.sh.sha256",
            "bazel-9.0.0-installer-linux-x86_64.sh.sig",
            "bazel-9.0.0-linux-arm64",
            "bazel-9.0.0-linux-arm64.sha256",
            "bazel-9.0.0-linux-arm64.sig",
            "bazel-9.0.0-linux-x86_64",
            "bazel-9.0.0-linux-x86_64.sha256",
            "bazel-9.0.0-linux-x86_64.sig",
            "bazel-9.0.0-windows-arm64.exe",
            "bazel-9.0.0-windows-arm64.exe.sha256",
            "bazel-9.0.0-windows-arm64.exe.sig",
            "bazel-9.0.0-windows-arm64.zip",
            "bazel-9.0.0-windows-arm64.zip.sha256",
            "bazel-9.0.0-windows-arm64.zip.sig",
            "bazel-9.0.0-windows-x86_64.exe",
            "bazel-9.0.0-windows-x86_64.exe.sha256",
            "bazel-9.0.0-windows-x86_64.exe.sig",
            "bazel-9.0.0-windows-x86_64.zip",
            "bazel-9.0.0-windows-x86_64.zip.sha256",
            "bazel-9.0.0-windows-x86_64.zip.sig",
            "bazel_9.0.0-linux-x86_64.deb",
            "bazel_9.0.0-linux-x86_64.deb.sha256",
            "bazel_9.0.0-linux-x86_64.deb.sig",
            "bazel_nojdk-9.0.0-darwin-arm64",
            "bazel_nojdk-9.0.0-darwin-arm64.sha256",
            "bazel_nojdk-9.0.0-darwin-arm64.sig",
            "bazel_nojdk-9.0.0-darwin-x86_64",
            "bazel_nojdk-9.0.0-darwin-x86_64.sha256",
            "bazel_nojdk-9.0.0-darwin-x86_64.sig",
            "bazel_nojdk-9.0.0-linux-arm64",
            "bazel_nojdk-9.0.0-linux-arm64.sha256",
            "bazel_nojdk-9.0.0-linux-arm64.sig",
            "bazel_nojdk-9.0.0-linux-x86_64",
            "bazel_nojdk-9.0.0-linux-x86_64.sha256",
            "bazel_nojdk-9.0.0-linux-x86_64.sig",
            "bazel_nojdk-9.0.0-windows-arm64.exe",
            "bazel_nojdk-9.0.0-windows-arm64.exe.sha256",
            "bazel_nojdk-9.0.0-windows-arm64.exe.sig",
            "bazel_nojdk-9.0.0-windows-arm64.zip",
            "bazel_nojdk-9.0.0-windows-arm64.zip.sha256",
            "bazel_nojdk-9.0.0-windows-arm64.zip.sig",
            "bazel_nojdk-9.0.0-windows-x86_64.exe",
            "bazel_nojdk-9.0.0-windows-x86_64.exe.sha256",
            "bazel_nojdk-9.0.0-windows-x86_64.exe.sig",
            "bazel_nojdk-9.0.0-windows-x86_64.zip",
            "bazel_nojdk-9.0.0-windows-x86_64.zip.sha256",
            "bazel_nojdk-9.0.0-windows-x86_64.zip.sig",
        ]
    }

    #[test]
    fn test_bazelbuild_bazel_bazel_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 21),
                (Platform::LinuxAarch64, 18),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 33),
                (Platform::WinArm64, 27),
            ],
            &bazelbuild_bazel_bazel_names(),
            "bazel",
        );
    }

    fn becheran_mlc_mlc_names() -> Vec<&'static str> {
        vec![
            "mlc-aarch64-apple-darwin",
            "mlc-aarch64-linux",
            "mlc-x86_64-linux",
            "mlc-x86_64-windows.exe",
        ]
    }

    #[test]
    fn test_becheran_mlc_mlc_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 1),
                (Platform::OsxArm64, 0),
            ],
            &becheran_mlc_mlc_names(),
            "mlc",
        );
    }

    fn benbjohnson_litestream_litestream_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "litestream-0.5.9-darwin-arm64.tar.gz",
            "litestream-0.5.9-darwin-arm64.tar.gz.sbom.json",
            "litestream-0.5.9-darwin-x86_64.tar.gz",
            "litestream-0.5.9-darwin-x86_64.tar.gz.sbom.json",
            "litestream-0.5.9-linux-arm64.deb",
            "litestream-0.5.9-linux-arm64.rpm",
            "litestream-0.5.9-linux-arm64.tar.gz",
            "litestream-0.5.9-linux-arm64.tar.gz.sbom.json",
            "litestream-0.5.9-linux-armv6.deb",
            "litestream-0.5.9-linux-armv6.rpm",
            "litestream-0.5.9-linux-armv6.tar.gz",
            "litestream-0.5.9-linux-armv6.tar.gz.sbom.json",
            "litestream-0.5.9-linux-armv7.deb",
            "litestream-0.5.9-linux-armv7.rpm",
            "litestream-0.5.9-linux-armv7.tar.gz",
            "litestream-0.5.9-linux-armv7.tar.gz.sbom.json",
            "litestream-0.5.9-linux-x86_64.deb",
            "litestream-0.5.9-linux-x86_64.rpm",
            "litestream-0.5.9-linux-x86_64.tar.gz",
            "litestream-0.5.9-linux-x86_64.tar.gz.sbom.json",
            "litestream-0.5.9-windows-arm64.zip",
            "litestream-0.5.9-windows-arm64.zip.sbom.json",
            "litestream-0.5.9-windows-x86_64.zip",
            "litestream-0.5.9-windows-x86_64.zip.sbom.json",
            "litestream-vfs-v0.5.9-darwin-amd64.tar.gz",
            "litestream-vfs-v0.5.9-darwin-amd64.tar.gz.sha256",
            "litestream-vfs-v0.5.9-darwin-arm64.tar.gz",
            "litestream-vfs-v0.5.9-darwin-arm64.tar.gz.sha256",
            "litestream-vfs-v0.5.9-linux-amd64.tar.gz",
            "litestream-vfs-v0.5.9-linux-amd64.tar.gz.sha256",
            "litestream-vfs-v0.5.9-linux-arm64.tar.gz",
            "litestream-vfs-v0.5.9-linux-arm64.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_benbjohnson_litestream_litestream_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 19),
                (Platform::LinuxAarch64, 7),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 23),
                (Platform::WinArm64, 21),
            ],
            &benbjohnson_litestream_litestream_names(),
            "litestream",
        );
    }

    fn birdayz_kaf_kaf_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "kaf_0.2.14_Darwin_arm64.tar.gz",
            "kaf_0.2.14_Darwin_x86_64.tar.gz",
            "kaf_0.2.14_Linux_arm64.tar.gz",
            "kaf_0.2.14_Linux_x86_64.tar.gz",
            "kaf_0.2.14_Windows_arm64.tar.gz",
            "kaf_0.2.14_Windows_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_birdayz_kaf_kaf_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
            ],
            &birdayz_kaf_kaf_names(),
            "kaf",
        );
    }

    fn bitnami_labs_sealed_secrets_kubeseal_names() -> Vec<&'static str> {
        vec![
            "controller-norbac.yaml",
            "controller.yaml",
            "cosign.pub",
            "kubeseal-0.36.0-darwin-amd64.tar.gz",
            "kubeseal-0.36.0-darwin-amd64.tar.gz.sig",
            "kubeseal-0.36.0-darwin-arm64.tar.gz",
            "kubeseal-0.36.0-darwin-arm64.tar.gz.sig",
            "kubeseal-0.36.0-linux-amd64.tar.gz",
            "kubeseal-0.36.0-linux-amd64.tar.gz.sig",
            "kubeseal-0.36.0-linux-arm.tar.gz",
            "kubeseal-0.36.0-linux-arm.tar.gz.sig",
            "kubeseal-0.36.0-linux-arm64.tar.gz",
            "kubeseal-0.36.0-linux-arm64.tar.gz.sig",
            "kubeseal-0.36.0-windows-amd64.tar.gz",
            "kubeseal-0.36.0-windows-amd64.tar.gz.sig",
            "sealed-secrets_0.36.0_checksums.txt",
            "sealed-secrets_0.36.0_checksums.txt.sig",
        ]
    }

    #[test]
    fn test_bitnami_labs_sealed_secrets_kubeseal_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 11),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 5),
                (Platform::Win64, 13),
            ],
            &bitnami_labs_sealed_secrets_kubeseal_names(),
            "kubeseal",
        );
    }

    fn block_goose_goose_names() -> Vec<&'static str> {
        vec![
            "download_cli.sh",
            "Goose-1.26.1-1.x86_64.rpm",
            "goose-aarch64-apple-darwin.tar.bz2",
            "goose-aarch64-unknown-linux-gnu.tar.bz2",
            "Goose-win32-x64.zip",
            "goose-x86_64-apple-darwin.tar.bz2",
            "goose-x86_64-pc-windows-msvc.zip",
            "goose-x86_64-unknown-linux-gnu.tar.bz2",
            "Goose.zip",
            "goose_1.26.1_amd64.deb",
            "Goose_intel_mac.zip",
            "io.github.block.Goose_stable_x86_64.flatpak",
        ]
    }

    #[test]
    fn test_block_goose_goose_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 2),
            ],
            &block_goose_goose_names(),
            "goose",
        );
    }

    fn bloznelis_typioca_typioca_names() -> Vec<&'static str> {
        vec![
            "typioca-linux-amd64",
            "typioca-mac-amd64",
            "typioca-mac-arm64",
            "typioca-win-amd64.exe",
            "typioca-win-arm64.exe",
        ]
    }

    #[test]
    fn test_bloznelis_typioca_typioca_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
            ],
            &bloznelis_typioca_typioca_names(),
            "typioca",
        );
    }

    fn bojand_ghz_ghz_names() -> Vec<&'static str> {
        vec![
            "ghz-darwin-arm64.tar.gz",
            "ghz-darwin-arm64.tar.gz.sha256",
            "ghz-darwin-x86_64.tar.gz",
            "ghz-darwin-x86_64.tar.gz.sha256",
            "ghz-linux-arm64.tar.gz",
            "ghz-linux-arm64.tar.gz.sha256",
            "ghz-linux-x86_64.tar.gz",
            "ghz-linux-x86_64.tar.gz.sha256",
            "ghz-windows-x86_64.zip",
            "ghz-windows-x86_64.zip.sha256",
        ]
    }

    #[test]
    fn test_bojand_ghz_ghz_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 8),
            ],
            &bojand_ghz_ghz_names(),
            "ghz",
        );
    }

    fn borgbackup_borg_borg_names() -> Vec<&'static str> {
        vec![
            "00_README.txt",
            "borg-freebsd-14-x86_64-gh",
            "borg-freebsd-14-x86_64-gh.tgz",
            "borg-linux-glibc231-x86_64",
            "borg-linux-glibc231-x86_64.asc",
            "borg-linux-glibc231-x86_64.tgz",
            "borg-linux-glibc231-x86_64.tgz.asc",
            "borg-linux-glibc235-arm64-gh",
            "borg-linux-glibc235-arm64-gh.tgz",
            "borg-linux-glibc235-x86_64-gh",
            "borg-linux-glibc235-x86_64-gh.tgz",
            "borg-macos-13-x86_64-gh",
            "borg-macos-13-x86_64-gh.tgz",
            "borg-macos-14-arm64-gh",
            "borg-macos-14-arm64-gh.tgz",
            "borgbackup-1.4.3.tar.gz",
            "borgbackup-1.4.3.tar.gz.asc",
        ]
    }

    #[test]
    fn test_borgbackup_borg_borg_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 7),
            ],
            &borgbackup_borg_borg_names(),
            "borg",
        );
    }

    fn br0xen_boltbrowser_boltbrowser_names() -> Vec<&'static str> {
        vec![
            "boltbrowser.darwin64",
            "boltbrowser.linux386",
            "boltbrowser.linux64",
            "boltbrowser.linuxarm",
            "boltbrowser.win386.exe",
            "boltbrowser.win64.exe",
        ]
    }

    #[test]
    fn test_br0xen_boltbrowser_boltbrowser_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 2),
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 0),
            ],
            &br0xen_boltbrowser_boltbrowser_names(),
            "boltbrowser",
        );
    }

    fn bridgecrewio_checkov_checkov_names() -> Vec<&'static str> {
        vec![
            "checkov_darwin_X86_64.zip",
            "checkov_linux_arm64.zip",
            "checkov_linux_X86_64.zip",
            "checkov_windows_X86_64.zip",
        ]
    }

    #[test]
    fn test_bridgecrewio_checkov_checkov_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 0),
                (Platform::Win64, 3),
            ],
            &bridgecrewio_checkov_checkov_names(),
            "checkov",
        );
    }

    fn bufbuild_buf_buf_names() -> Vec<&'static str> {
        vec![
            "buf-Darwin-arm64",
            "buf-Darwin-arm64.tar.gz",
            "buf-Darwin-x86_64",
            "buf-Darwin-x86_64.tar.gz",
            "buf-Linux-aarch64",
            "buf-Linux-aarch64.tar.gz",
            "buf-Linux-armv7",
            "buf-Linux-armv7.tar.gz",
            "buf-Linux-ppc64le",
            "buf-Linux-ppc64le.tar.gz",
            "buf-Linux-riscv64",
            "buf-Linux-riscv64.tar.gz",
            "buf-Linux-s390x",
            "buf-Linux-s390x.tar.gz",
            "buf-Linux-x86_64",
            "buf-Linux-x86_64.tar.gz",
            "buf-Windows-arm64.exe",
            "buf-Windows-arm64.zip",
            "buf-Windows-x86_64.exe",
            "buf-Windows-x86_64.zip",
            "protoc-gen-buf-breaking-Darwin-arm64",
            "protoc-gen-buf-breaking-Darwin-x86_64",
            "protoc-gen-buf-breaking-Linux-aarch64",
            "protoc-gen-buf-breaking-Linux-armv7",
            "protoc-gen-buf-breaking-Linux-ppc64le",
            "protoc-gen-buf-breaking-Linux-riscv64",
            "protoc-gen-buf-breaking-Linux-s390x",
            "protoc-gen-buf-breaking-Linux-x86_64",
            "protoc-gen-buf-breaking-Windows-arm64.exe",
            "protoc-gen-buf-breaking-Windows-x86_64.exe",
            "protoc-gen-buf-lint-Darwin-arm64",
            "protoc-gen-buf-lint-Darwin-x86_64",
            "protoc-gen-buf-lint-Linux-aarch64",
            "protoc-gen-buf-lint-Linux-armv7",
            "protoc-gen-buf-lint-Linux-ppc64le",
            "protoc-gen-buf-lint-Linux-riscv64",
            "protoc-gen-buf-lint-Linux-s390x",
            "protoc-gen-buf-lint-Linux-x86_64",
            "protoc-gen-buf-lint-Windows-arm64.exe",
            "protoc-gen-buf-lint-Windows-x86_64.exe",
            "sha256.txt",
            "sha256.txt.minisig",
        ]
    }

    #[test]
    fn test_bufbuild_buf_buf_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 15),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 19),
                (Platform::WinArm64, 17),
            ],
            &bufbuild_buf_buf_names(),
            "buf",
        );
    }

    fn buildkite_cli_bk_names() -> Vec<&'static str> {
        vec![
            "bk_3.30.0_checksums.txt",
            "bk_3.30.0_linux_amd64.apk",
            "bk_3.30.0_linux_amd64.deb",
            "bk_3.30.0_linux_amd64.rpm",
            "bk_3.30.0_linux_amd64.tar.gz",
            "bk_3.30.0_linux_arm64.apk",
            "bk_3.30.0_linux_arm64.deb",
            "bk_3.30.0_linux_arm64.rpm",
            "bk_3.30.0_linux_arm64.tar.gz",
            "bk_3.30.0_macOS_amd64.zip",
            "bk_3.30.0_macOS_arm64.zip",
            "bk_3.30.0_windows_amd64.zip",
            "bk_3.30.0_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_buildkite_cli_bk_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 8),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 10),
                (Platform::Win64, 11),
                (Platform::WinArm64, 12),
            ],
            &buildkite_cli_bk_names(),
            "bk",
        );
    }

    fn buildpacks_pack_pack_names() -> Vec<&'static str> {
        vec![
            "pack-v0.40.1-freebsd-arm64.tgz",
            "pack-v0.40.1-freebsd-arm64.tgz.sha256",
            "pack-v0.40.1-freebsd.tgz",
            "pack-v0.40.1-freebsd.tgz.sha256",
            "pack-v0.40.1-linux-arm64.tgz",
            "pack-v0.40.1-linux-arm64.tgz.sha256",
            "pack-v0.40.1-linux-ppc64le.tgz",
            "pack-v0.40.1-linux-ppc64le.tgz.sha256",
            "pack-v0.40.1-linux-s390x.tgz",
            "pack-v0.40.1-linux-s390x.tgz.sha256",
            "pack-v0.40.1-linux.tgz",
            "pack-v0.40.1-linux.tgz.sha256",
            "pack-v0.40.1-macos-arm64.tgz",
            "pack-v0.40.1-macos-arm64.tgz.sha256",
            "pack-v0.40.1-macos.tgz",
            "pack-v0.40.1-macos.tgz.sha256",
            "pack-v0.40.1-windows.zip",
            "pack-v0.40.1-windows.zip.sha256",
        ]
    }

    #[test]
    fn test_buildpacks_pack_pack_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 10),
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 14),
                (Platform::OsxArm64, 12),
                (Platform::Win32, 16),
                (Platform::Win64, 16),
                (Platform::WinArm64, 16),
            ],
            &buildpacks_pack_pack_names(),
            "pack",
        );
    }

    fn busser_tftree_tftree_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "tftree_0.2.0_darwin_amd64.tar.gz",
            "tftree_0.2.0_darwin_arm64.tar.gz",
            "tftree_0.2.0_linux_386.tar.gz",
            "tftree_0.2.0_linux_amd64.tar.gz",
            "tftree_0.2.0_linux_arm64.tar.gz",
            "tftree_0.2.0_windows_386.zip",
            "tftree_0.2.0_windows_amd64.zip",
            "tftree_0.2.0_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_busser_tftree_tftree_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 3),
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win32, 6),
                (Platform::Win64, 7),
                (Platform::WinArm64, 8),
            ],
            &busser_tftree_tftree_names(),
            "tftree",
        );
    }

    fn bvaisvil_zenith_zenith_names() -> Vec<&'static str> {
        vec![
            "zenith-Linux-gnu-x86_64.tar.gz",
            "zenith-Linux-gnu-x86_64.tar.gz.sha256",
            "zenith-Linux-musl-arm64.tar.gz",
            "zenith-Linux-musl-arm64.tar.gz.sha256",
            "zenith-Linux-musl-x86_64.tar.gz",
            "zenith-Linux-musl-x86_64.tar.gz.sha256",
            "zenith-macOS-arm64.tar.gz",
            "zenith-macOS-arm64.tar.gz.sha256",
            "zenith-macOS-x86_64.tar.gz",
            "zenith-macOS-x86_64.tar.gz.sha256",
            "zenith_0.14.3-1_amd64.deb",
            "zenith_0.14.3-1_amd64.deb.sha256",
        ]
    }

    #[test]
    fn test_bvaisvil_zenith_zenith_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 8),
                (Platform::OsxArm64, 6),
            ],
            &bvaisvil_zenith_zenith_names(),
            "zenith",
        );
    }

    fn bytecodealliance_wasm_pkg_tools_wkg_names() -> Vec<&'static str> {
        vec![
            "wkg-aarch64-apple-darwin",
            "wkg-aarch64-unknown-linux-gnu",
            "wkg-x86_64-apple-darwin",
            "wkg-x86_64-pc-windows-gnu",
            "wkg-x86_64-unknown-linux-gnu",
        ]
    }

    #[test]
    fn test_bytecodealliance_wasm_pkg_tools_wkg_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 3),
            ],
            &bytecodealliance_wasm_pkg_tools_wkg_names(),
            "wkg",
        );
    }

    fn bytecodealliance_wasm_tools_wasm_tools_names() -> Vec<&'static str> {
        vec![
            "wasm-tools-1.245.1-aarch64-linux.tar.gz",
            "wasm-tools-1.245.1-aarch64-macos.tar.gz",
            "wasm-tools-1.245.1-aarch64-musl.tar.gz",
            "wasm-tools-1.245.1-aarch64-windows.zip",
            "wasm-tools-1.245.1-wasm32-wasip1.tar.gz",
            "wasm-tools-1.245.1-x86_64-linux.tar.gz",
            "wasm-tools-1.245.1-x86_64-macos.tar.gz",
            "wasm-tools-1.245.1-x86_64-musl.tar.gz",
            "wasm-tools-1.245.1-x86_64-windows.zip",
        ]
    }

    #[test]
    fn test_bytecodealliance_wasm_tools_wasm_tools_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 8),
                (Platform::WinArm64, 3),
            ],
            &bytecodealliance_wasm_tools_wasm_tools_names(),
            "wasm-tools",
        );
    }

    fn bytecodealliance_wasmtime_wasmtime_names() -> Vec<&'static str> {
        vec![
            "wasi_snapshot_preview1.command.wasm",
            "wasi_snapshot_preview1.proxy.wasm",
            "wasi_snapshot_preview1.reactor.wasm",
            "wasmtime-platform.h",
            "wasmtime-v42.0.1-aarch64-android-c-api.tar.xz",
            "wasmtime-v42.0.1-aarch64-android.tar.xz",
            "wasmtime-v42.0.1-aarch64-linux-c-api.tar.xz",
            "wasmtime-v42.0.1-aarch64-linux.tar.xz",
            "wasmtime-v42.0.1-aarch64-macos-c-api.tar.xz",
            "wasmtime-v42.0.1-aarch64-macos.tar.xz",
            "wasmtime-v42.0.1-aarch64-musl-c-api.tar.xz",
            "wasmtime-v42.0.1-aarch64-musl.tar.xz",
            "wasmtime-v42.0.1-aarch64-windows-c-api.zip",
            "wasmtime-v42.0.1-aarch64-windows.zip",
            "wasmtime-v42.0.1-armv7-linux-c-api.tar.xz",
            "wasmtime-v42.0.1-armv7-linux.tar.xz",
            "wasmtime-v42.0.1-i686-linux-c-api.tar.xz",
            "wasmtime-v42.0.1-i686-linux.tar.xz",
            "wasmtime-v42.0.1-i686-windows-c-api.zip",
            "wasmtime-v42.0.1-i686-windows.zip",
            "wasmtime-v42.0.1-riscv64gc-linux-c-api.tar.xz",
            "wasmtime-v42.0.1-riscv64gc-linux.tar.xz",
            "wasmtime-v42.0.1-s390x-linux-c-api.tar.xz",
            "wasmtime-v42.0.1-s390x-linux.tar.xz",
            "wasmtime-v42.0.1-src.tar.gz",
            "wasmtime-v42.0.1-x86_64-android-c-api.tar.xz",
            "wasmtime-v42.0.1-x86_64-android.tar.xz",
            "wasmtime-v42.0.1-x86_64-linux-c-api.tar.xz",
            "wasmtime-v42.0.1-x86_64-linux.tar.xz",
            "wasmtime-v42.0.1-x86_64-macos-c-api.tar.xz",
            "wasmtime-v42.0.1-x86_64-macos.tar.xz",
            "wasmtime-v42.0.1-x86_64-mingw-c-api.zip",
            "wasmtime-v42.0.1-x86_64-mingw.zip",
            "wasmtime-v42.0.1-x86_64-musl-c-api.tar.xz",
            "wasmtime-v42.0.1-x86_64-musl.tar.xz",
            "wasmtime-v42.0.1-x86_64-windows-c-api.zip",
            "wasmtime-v42.0.1-x86_64-windows.msi",
            "wasmtime-v42.0.1-x86_64-windows.zip",
        ]
    }

    #[test]
    fn test_bytecodealliance_wasmtime_wasmtime_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 28),
                (Platform::LinuxAarch64, 7),
                (Platform::Osx64, 30),
                (Platform::OsxArm64, 9),
                (Platform::Win64, 37),
                (Platform::WinArm64, 13),
            ],
            &bytecodealliance_wasmtime_wasmtime_names(),
            "wasmtime",
        );
    }

    fn caarlos0_fork_cleaner_fork_cleaner_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "checksums.txt.sigstore.json",
            "fork-cleaner_2.4.0_darwin_all.tar.gz",
            "fork-cleaner_2.4.0_darwin_all.tar.gz.sbom.json",
            "fork-cleaner_2.4.0_linux_amd64.apk",
            "fork-cleaner_2.4.0_linux_amd64.deb",
            "fork-cleaner_2.4.0_linux_amd64.rpm",
            "fork-cleaner_2.4.0_linux_amd64.tar.gz",
            "fork-cleaner_2.4.0_linux_amd64.tar.gz.sbom.json",
            "fork-cleaner_2.4.0_linux_arm64.apk",
            "fork-cleaner_2.4.0_linux_arm64.deb",
            "fork-cleaner_2.4.0_linux_arm64.rpm",
            "fork-cleaner_2.4.0_linux_arm64.tar.gz",
            "fork-cleaner_2.4.0_linux_arm64.tar.gz.sbom.json",
            "fork-cleaner_2.4.0_windows_amd64.zip",
            "fork-cleaner_2.4.0_windows_amd64.zip.sbom.json",
            "fork-cleaner_2.4.0_windows_arm64.zip",
            "fork-cleaner_2.4.0_windows_arm64.zip.sbom.json",
        ]
    }

    #[test]
    fn test_caarlos0_fork_cleaner_fork_cleaner_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 12),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 14),
                (Platform::WinArm64, 16),
            ],
            &caarlos0_fork_cleaner_fork_cleaner_names(),
            "fork-cleaner",
        );
    }

    fn caddyserver_caddy_caddy_names() -> Vec<&'static str> {
        vec![
            "caddy_2.11.1_buildable-artifact.pem",
            "caddy_2.11.1_buildable-artifact.tar.gz",
            "caddy_2.11.1_buildable-artifact.tar.gz.sig",
            "caddy_2.11.1_checksums.txt",
            "caddy_2.11.1_checksums.txt.pem",
            "caddy_2.11.1_checksums.txt.sig",
            "caddy_2.11.1_freebsd_amd64.pem",
            "caddy_2.11.1_freebsd_amd64.sbom",
            "caddy_2.11.1_freebsd_amd64.sbom.pem",
            "caddy_2.11.1_freebsd_amd64.sbom.sig",
            "caddy_2.11.1_freebsd_amd64.tar.gz",
            "caddy_2.11.1_freebsd_amd64.tar.gz.sig",
            "caddy_2.11.1_freebsd_arm64.pem",
            "caddy_2.11.1_freebsd_arm64.sbom",
            "caddy_2.11.1_freebsd_arm64.sbom.pem",
            "caddy_2.11.1_freebsd_arm64.sbom.sig",
            "caddy_2.11.1_freebsd_arm64.tar.gz",
            "caddy_2.11.1_freebsd_arm64.tar.gz.sig",
            "caddy_2.11.1_freebsd_armv6.pem",
            "caddy_2.11.1_freebsd_armv6.sbom",
            "caddy_2.11.1_freebsd_armv6.sbom.pem",
            "caddy_2.11.1_freebsd_armv6.sbom.sig",
            "caddy_2.11.1_freebsd_armv6.tar.gz",
            "caddy_2.11.1_freebsd_armv6.tar.gz.sig",
            "caddy_2.11.1_freebsd_armv7.pem",
            "caddy_2.11.1_freebsd_armv7.sbom",
            "caddy_2.11.1_freebsd_armv7.sbom.pem",
            "caddy_2.11.1_freebsd_armv7.sbom.sig",
            "caddy_2.11.1_freebsd_armv7.tar.gz",
            "caddy_2.11.1_freebsd_armv7.tar.gz.sig",
            "caddy_2.11.1_linux_amd64.deb",
            "caddy_2.11.1_linux_amd64.deb.pem",
            "caddy_2.11.1_linux_amd64.deb.sig",
            "caddy_2.11.1_linux_amd64.pem",
            "caddy_2.11.1_linux_amd64.sbom",
            "caddy_2.11.1_linux_amd64.sbom.pem",
            "caddy_2.11.1_linux_amd64.sbom.sig",
            "caddy_2.11.1_linux_amd64.tar.gz",
            "caddy_2.11.1_linux_amd64.tar.gz.sig",
            "caddy_2.11.1_linux_arm64.deb",
            "caddy_2.11.1_linux_arm64.deb.pem",
            "caddy_2.11.1_linux_arm64.deb.sig",
            "caddy_2.11.1_linux_arm64.pem",
            "caddy_2.11.1_linux_arm64.sbom",
            "caddy_2.11.1_linux_arm64.sbom.pem",
            "caddy_2.11.1_linux_arm64.sbom.sig",
            "caddy_2.11.1_linux_arm64.tar.gz",
            "caddy_2.11.1_linux_arm64.tar.gz.sig",
            "caddy_2.11.1_linux_armv5.deb",
            "caddy_2.11.1_linux_armv5.deb.pem",
            "caddy_2.11.1_linux_armv5.deb.sig",
            "caddy_2.11.1_linux_armv5.pem",
            "caddy_2.11.1_linux_armv5.sbom",
            "caddy_2.11.1_linux_armv5.sbom.pem",
            "caddy_2.11.1_linux_armv5.sbom.sig",
            "caddy_2.11.1_linux_armv5.tar.gz",
            "caddy_2.11.1_linux_armv5.tar.gz.sig",
            "caddy_2.11.1_linux_armv6.deb",
            "caddy_2.11.1_linux_armv6.deb.pem",
            "caddy_2.11.1_linux_armv6.deb.sig",
            "caddy_2.11.1_linux_armv6.pem",
            "caddy_2.11.1_linux_armv6.sbom",
            "caddy_2.11.1_linux_armv6.sbom.pem",
            "caddy_2.11.1_linux_armv6.sbom.sig",
            "caddy_2.11.1_linux_armv6.tar.gz",
            "caddy_2.11.1_linux_armv6.tar.gz.sig",
            "caddy_2.11.1_linux_armv7.deb",
            "caddy_2.11.1_linux_armv7.deb.pem",
            "caddy_2.11.1_linux_armv7.deb.sig",
            "caddy_2.11.1_linux_armv7.pem",
            "caddy_2.11.1_linux_armv7.sbom",
            "caddy_2.11.1_linux_armv7.sbom.pem",
            "caddy_2.11.1_linux_armv7.sbom.sig",
            "caddy_2.11.1_linux_armv7.tar.gz",
            "caddy_2.11.1_linux_armv7.tar.gz.sig",
            "caddy_2.11.1_linux_ppc64le.deb",
            "caddy_2.11.1_linux_ppc64le.deb.pem",
            "caddy_2.11.1_linux_ppc64le.deb.sig",
            "caddy_2.11.1_linux_ppc64le.pem",
            "caddy_2.11.1_linux_ppc64le.sbom",
            "caddy_2.11.1_linux_ppc64le.sbom.pem",
            "caddy_2.11.1_linux_ppc64le.sbom.sig",
            "caddy_2.11.1_linux_ppc64le.tar.gz",
            "caddy_2.11.1_linux_ppc64le.tar.gz.sig",
            "caddy_2.11.1_linux_riscv64.deb",
            "caddy_2.11.1_linux_riscv64.deb.pem",
            "caddy_2.11.1_linux_riscv64.deb.sig",
            "caddy_2.11.1_linux_riscv64.pem",
            "caddy_2.11.1_linux_riscv64.sbom",
            "caddy_2.11.1_linux_riscv64.sbom.pem",
            "caddy_2.11.1_linux_riscv64.sbom.sig",
            "caddy_2.11.1_linux_riscv64.tar.gz",
            "caddy_2.11.1_linux_riscv64.tar.gz.sig",
            "caddy_2.11.1_linux_s390x.deb",
            "caddy_2.11.1_linux_s390x.deb.pem",
            "caddy_2.11.1_linux_s390x.deb.sig",
            "caddy_2.11.1_linux_s390x.pem",
            "caddy_2.11.1_linux_s390x.sbom",
            "caddy_2.11.1_linux_s390x.sbom.pem",
            "caddy_2.11.1_linux_s390x.sbom.sig",
            "caddy_2.11.1_linux_s390x.tar.gz",
            "caddy_2.11.1_linux_s390x.tar.gz.sig",
            "caddy_2.11.1_mac_amd64.pem",
            "caddy_2.11.1_mac_amd64.sbom",
            "caddy_2.11.1_mac_amd64.sbom.pem",
            "caddy_2.11.1_mac_amd64.sbom.sig",
            "caddy_2.11.1_mac_amd64.tar.gz",
            "caddy_2.11.1_mac_amd64.tar.gz.sig",
            "caddy_2.11.1_mac_arm64.pem",
            "caddy_2.11.1_mac_arm64.sbom",
            "caddy_2.11.1_mac_arm64.sbom.pem",
            "caddy_2.11.1_mac_arm64.sbom.sig",
            "caddy_2.11.1_mac_arm64.tar.gz",
            "caddy_2.11.1_mac_arm64.tar.gz.sig",
            "caddy_2.11.1_src.pem",
            "caddy_2.11.1_src.tar.gz",
            "caddy_2.11.1_src.tar.gz.sig",
            "caddy_2.11.1_windows_amd64.pem",
            "caddy_2.11.1_windows_amd64.sbom",
            "caddy_2.11.1_windows_amd64.sbom.pem",
            "caddy_2.11.1_windows_amd64.sbom.sig",
            "caddy_2.11.1_windows_amd64.zip",
            "caddy_2.11.1_windows_amd64.zip.sig",
            "caddy_2.11.1_windows_arm64.pem",
            "caddy_2.11.1_windows_arm64.sbom",
            "caddy_2.11.1_windows_arm64.sbom.pem",
            "caddy_2.11.1_windows_arm64.sbom.sig",
            "caddy_2.11.1_windows_arm64.zip",
            "caddy_2.11.1_windows_arm64.zip.sig",
        ]
    }

    #[test]
    fn test_caddyserver_caddy_caddy_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 37),
                (Platform::LinuxAarch64, 46),
                (Platform::Osx64, 106),
                (Platform::OsxArm64, 112),
                (Platform::Win64, 121),
                (Platform::WinArm64, 127),
            ],
            &caddyserver_caddy_caddy_names(),
            "caddy",
        );
    }

    fn cameron_martin_bazel_lsp_bazel_lsp_names() -> Vec<&'static str> {
        vec![
            "bazel-lsp-0.6.4-linux-amd64",
            "bazel-lsp-0.6.4-linux-arm64",
            "bazel-lsp-0.6.4-osx-amd64",
            "bazel-lsp-0.6.4-osx-arm64",
            "bazel-lsp-0.6.4-windows-amd64.exe",
        ]
    }

    #[test]
    fn test_cameron_martin_bazel_lsp_bazel_lsp_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 3),
            ],
            &cameron_martin_bazel_lsp_bazel_lsp_names(),
            "bazel-lsp",
        );
    }

    fn cargo_bins_cargo_binstall_cargo_binstall_names() -> Vec<&'static str> {
        vec![
            "cargo-binstall-aarch64-apple-darwin.full.zip",
            "cargo-binstall-aarch64-apple-darwin.full.zip.sig",
            "cargo-binstall-aarch64-apple-darwin.zip",
            "cargo-binstall-aarch64-apple-darwin.zip.sig",
            "cargo-binstall-aarch64-pc-windows-msvc.full.zip",
            "cargo-binstall-aarch64-pc-windows-msvc.full.zip.sig",
            "cargo-binstall-aarch64-pc-windows-msvc.zip",
            "cargo-binstall-aarch64-pc-windows-msvc.zip.sig",
            "cargo-binstall-aarch64-unknown-linux-gnu.full.tgz",
            "cargo-binstall-aarch64-unknown-linux-gnu.full.tgz.sig",
            "cargo-binstall-aarch64-unknown-linux-gnu.tgz",
            "cargo-binstall-aarch64-unknown-linux-gnu.tgz.sig",
            "cargo-binstall-aarch64-unknown-linux-musl.full.tgz",
            "cargo-binstall-aarch64-unknown-linux-musl.full.tgz.sig",
            "cargo-binstall-aarch64-unknown-linux-musl.tgz",
            "cargo-binstall-aarch64-unknown-linux-musl.tgz.sig",
            "cargo-binstall-armv7-unknown-linux-gnueabihf.full.tgz",
            "cargo-binstall-armv7-unknown-linux-gnueabihf.full.tgz.sig",
            "cargo-binstall-armv7-unknown-linux-gnueabihf.tgz",
            "cargo-binstall-armv7-unknown-linux-gnueabihf.tgz.sig",
            "cargo-binstall-armv7-unknown-linux-musleabihf.full.tgz",
            "cargo-binstall-armv7-unknown-linux-musleabihf.full.tgz.sig",
            "cargo-binstall-armv7-unknown-linux-musleabihf.tgz",
            "cargo-binstall-armv7-unknown-linux-musleabihf.tgz.sig",
            "cargo-binstall-universal-apple-darwin.full.zip",
            "cargo-binstall-universal-apple-darwin.full.zip.sig",
            "cargo-binstall-universal-apple-darwin.zip",
            "cargo-binstall-universal-apple-darwin.zip.sig",
            "cargo-binstall-x86_64-apple-darwin.full.zip",
            "cargo-binstall-x86_64-apple-darwin.full.zip.sig",
            "cargo-binstall-x86_64-apple-darwin.zip",
            "cargo-binstall-x86_64-apple-darwin.zip.sig",
            "cargo-binstall-x86_64-pc-windows-msvc.full.zip",
            "cargo-binstall-x86_64-pc-windows-msvc.full.zip.sig",
            "cargo-binstall-x86_64-pc-windows-msvc.zip",
            "cargo-binstall-x86_64-pc-windows-msvc.zip.sig",
            "cargo-binstall-x86_64-unknown-linux-gnu.full.tgz",
            "cargo-binstall-x86_64-unknown-linux-gnu.full.tgz.sig",
            "cargo-binstall-x86_64-unknown-linux-gnu.tgz",
            "cargo-binstall-x86_64-unknown-linux-gnu.tgz.sig",
            "cargo-binstall-x86_64-unknown-linux-musl.full.tgz",
            "cargo-binstall-x86_64-unknown-linux-musl.full.tgz.sig",
            "cargo-binstall-x86_64-unknown-linux-musl.tgz",
            "cargo-binstall-x86_64-unknown-linux-musl.tgz.sig",
            "minisign.pub",
        ]
    }

    #[test]
    fn test_cargo_bins_cargo_binstall_cargo_binstall_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 42),
                (Platform::LinuxAarch64, 14),
                (Platform::Osx64, 30),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 34),
                (Platform::WinArm64, 6),
            ],
            &cargo_bins_cargo_binstall_cargo_binstall_names(),
            "cargo-binstall",
        );
    }

    fn carthage_software_mago_mago_names() -> Vec<&'static str> {
        vec![
            "mago-1.13.3-aarch64-apple-darwin.tar.gz",
            "mago-1.13.3-aarch64-unknown-linux-gnu.tar.gz",
            "mago-1.13.3-aarch64-unknown-linux-musl.tar.gz",
            "mago-1.13.3-arm-unknown-linux-gnueabi.tar.gz",
            "mago-1.13.3-arm-unknown-linux-gnueabihf.tar.gz",
            "mago-1.13.3-arm-unknown-linux-musleabi.tar.gz",
            "mago-1.13.3-arm-unknown-linux-musleabihf.tar.gz",
            "mago-1.13.3-armv7-unknown-linux-gnueabihf.tar.gz",
            "mago-1.13.3-armv7-unknown-linux-musleabihf.tar.gz",
            "mago-1.13.3-wasm.tar.gz",
            "mago-1.13.3-x86_64-apple-darwin.tar.gz",
            "mago-1.13.3-x86_64-pc-windows-gnu.tar.gz",
            "mago-1.13.3-x86_64-pc-windows-msvc.zip",
            "mago-1.13.3-x86_64-unknown-freebsd.tar.gz",
            "mago-1.13.3-x86_64-unknown-linux-gnu.tar.gz",
            "mago-1.13.3-x86_64-unknown-linux-musl.tar.gz",
            "source-code.tar.gz",
            "source-code.zip",
        ]
    }

    #[test]
    fn test_carthage_software_mago_mago_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 15),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 12),
            ],
            &carthage_software_mago_mago_names(),
            "mago",
        );
    }

    fn casey_just_just_names() -> Vec<&'static str> {
        vec![
            "CHANGELOG.md",
            "just-1.46.0-aarch64-apple-darwin.tar.gz",
            "just-1.46.0-aarch64-pc-windows-msvc.zip",
            "just-1.46.0-aarch64-unknown-linux-musl.tar.gz",
            "just-1.46.0-arm-unknown-linux-musleabihf.tar.gz",
            "just-1.46.0-armv7-unknown-linux-musleabihf.tar.gz",
            "just-1.46.0-loongarch64-unknown-linux-musl.tar.gz",
            "just-1.46.0-x86_64-apple-darwin.tar.gz",
            "just-1.46.0-x86_64-pc-windows-msvc.zip",
            "just-1.46.0-x86_64-unknown-linux-musl.tar.gz",
            "SHA256SUMS",
        ]
    }

    #[test]
    fn test_casey_just_just_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 8),
                (Platform::WinArm64, 2),
            ],
            &casey_just_just_names(),
            "just",
        );
    }

    fn cea_hpc_sshproxy_sshproxy_names() -> Vec<&'static str> {
        vec![
            "sshproxy-2.1.0-1.fc42.src.rpm",
            "sshproxy-2.1.0-1.fc42.x86_64.rpm",
            "sshproxy_2.1.0_Linux_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_cea_hpc_sshproxy_sshproxy_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
            ],
            &cea_hpc_sshproxy_sshproxy_names(),
            "sshproxy",
        );
    }

    fn chaaz_versio_versio_names() -> Vec<&'static str> {
        vec![
            "versio__x86_64-apple-darwin",
            "versio__x86_64-pc-windows-msvc",
            "versio__x86_64-unknown-linux-gnu",
        ]
    }

    #[test]
    fn test_chaaz_versio_versio_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 0),
                (Platform::Win64, 1),
            ],
            &chaaz_versio_versio_names(),
            "versio",
        );
    }

    fn chaqchase_lla_lla_names() -> Vec<&'static str> {
        vec![
            "lla-0.5.4-1-aarch64.pkg.tar.zst",
            "lla-0.5.4-1-i686.pkg.tar.zst",
            "lla-0.5.4-1-x86_64.pkg.tar.zst",
            "lla-0.5.4-1.aarch64.rpm",
            "lla-0.5.4-1.i686.rpm",
            "lla-0.5.4-1.x86_64.rpm",
            "lla-0.5.4-r0.aarch64.apk",
            "lla-0.5.4-r0.x86.apk",
            "lla-0.5.4-r0.x86_64.apk",
            "lla-linux-amd64",
            "lla-linux-arm64",
            "lla-linux-i686",
            "lla-macos-amd64",
            "lla-macos-arm64",
            "lla_0.5.4_amd64.deb",
            "lla_0.5.4_arm64.deb",
            "lla_0.5.4_i386.deb",
            "plugins-linux-amd64.tar.gz",
            "plugins-linux-arm64.tar.gz",
            "plugins-linux-i686.tar.gz",
            "plugins-macos-amd64.tar.gz",
            "plugins-macos-arm64.tar.gz",
            "SHA256SUMS",
            "themes.zip",
        ]
    }

    #[test]
    fn test_chaqchase_lla_lla_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 10),
                (Platform::Osx64, 12),
                (Platform::OsxArm64, 13),
            ],
            &chaqchase_lla_lla_names(),
            "lla",
        );
    }

    fn che_incubator_chectl_chectl_names() -> Vec<&'static str> {
        vec![
            "chectl-darwin-arm64.tar.gz",
            "chectl-darwin-x64.tar.gz",
            "chectl-linux-arm.tar.gz",
            "chectl-linux-arm64.tar.gz",
            "chectl-linux-ppc64le.tar.gz",
            "chectl-linux-s390x.tar.gz",
            "chectl-linux-x64.tar.gz",
            "chectl-win32-x64.tar.gz",
            "chectl-win32-x86.tar.gz",
        ]
    }

    #[test]
    fn test_che_incubator_chectl_chectl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
            ],
            &che_incubator_chectl_chectl_names(),
            "chectl",
        );
    }

    fn chmouel_snazy_snazy_v_names() -> Vec<&'static str> {
        vec![
            "snazy-v0.58.1-linux-amd64.tar.gz",
            "snazy-v0.58.1-linux-amd64.tar.gz.sha256",
            "snazy-v0.58.1-linux-arm.tar.gz",
            "snazy-v0.58.1-linux-arm.tar.gz.sha256",
            "snazy-v0.58.1-linux-arm64-musl.tar.gz",
            "snazy-v0.58.1-linux-arm64-musl.tar.gz.sha256",
            "snazy-v0.58.1-linux-arm64.tar.gz",
            "snazy-v0.58.1-linux-arm64.tar.gz.sha256",
            "snazy-v0.58.1-macos-arm64.tar.gz",
            "snazy-v0.58.1-macos-arm64.tar.gz.sha256",
            "snazy-v0.58.1-macos.tar.gz",
            "snazy-v0.58.1-macos.tar.gz.sha256",
            "snazy-v0.58.1-windows.zip",
            "snazy-v0.58.1-windows.zip.sha256",
        ]
    }

    #[test]
    fn test_chmouel_snazy_snazy_v_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 10),
                (Platform::Win32, 12),
                (Platform::Win64, 12),
                (Platform::WinArm64, 12),
            ],
            &chmouel_snazy_snazy_v_names(),
            "snazy-v",
        );
    }

    fn chriswalz_bit_bit_names() -> Vec<&'static str> {
        vec![
            "bit_1.1.2_checksums.txt",
            "bit_1.1.2_darwin_amd64.tar.gz",
            "bit_1.1.2_darwin_arm64.tar.gz",
            "bit_1.1.2_linux_386.tar.gz",
            "bit_1.1.2_linux_amd64.tar.gz",
            "bit_1.1.2_linux_arm64.tar.gz",
            "bit_1.1.2_netbsd_386.tar.gz",
            "bit_1.1.2_netbsd_amd64.tar.gz",
            "bit_1.1.2_windows_386.tar.gz",
            "bit_1.1.2_windows_amd64.tar.gz",
        ]
    }

    #[test]
    fn test_chriswalz_bit_bit_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 3),
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win32, 8),
                (Platform::Win64, 9),
            ],
            &chriswalz_bit_bit_names(),
            "bit",
        );
    }

    fn cilium_hubble_hubble_names() -> Vec<&'static str> {
        vec![
            "hubble-darwin-amd64.tar.gz",
            "hubble-darwin-amd64.tar.gz.sha256sum",
            "hubble-darwin-arm64.tar.gz",
            "hubble-darwin-arm64.tar.gz.sha256sum",
            "hubble-linux-amd64.tar.gz",
            "hubble-linux-amd64.tar.gz.sha256sum",
            "hubble-linux-arm64.tar.gz",
            "hubble-linux-arm64.tar.gz.sha256sum",
            "hubble-windows-amd64.tar.gz",
            "hubble-windows-amd64.tar.gz.sha256sum",
            "hubble-windows-arm64.tar.gz",
            "hubble-windows-arm64.tar.gz.sha256sum",
        ]
    }

    #[test]
    fn test_cilium_hubble_hubble_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 8),
                (Platform::WinArm64, 10),
            ],
            &cilium_hubble_hubble_names(),
            "hubble",
        );
    }

    fn citrusframework_yaks_yaks_names() -> Vec<&'static str> {
        vec![
            "yaks-0.20.0-linux-64bit.tar.gz",
            "yaks-0.20.0-mac-64bit.tar.gz",
            "yaks-0.20.0-mac-arm64bit.tar.gz",
            "yaks-0.20.0-windows-64bit.tar.gz",
        ]
    }

    #[test]
    fn test_citrusframework_yaks_yaks_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::Win64, 3),
            ],
            &citrusframework_yaks_yaks_names(),
            "yaks",
        );
    }

    fn ck_zhang_reddix_reddix_names() -> Vec<&'static str> {
        vec![
            "dist-manifest.json",
            "reddix-aarch64-apple-darwin.tar.xz",
            "reddix-aarch64-apple-darwin.tar.xz.sha256",
            "reddix-aarch64-unknown-linux-gnu.tar.xz",
            "reddix-aarch64-unknown-linux-gnu.tar.xz.sha256",
            "reddix-installer.ps1",
            "reddix-installer.sh",
            "reddix-x86_64-apple-darwin.tar.xz",
            "reddix-x86_64-apple-darwin.tar.xz.sha256",
            "reddix-x86_64-pc-windows-msvc.zip",
            "reddix-x86_64-pc-windows-msvc.zip.sha256",
            "reddix-x86_64-unknown-linux-gnu.tar.xz",
            "reddix-x86_64-unknown-linux-gnu.tar.xz.sha256",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_ck_zhang_reddix_reddix_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 11),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 9),
            ],
            &ck_zhang_reddix_reddix_names(),
            "reddix",
        );
    }

    fn clamoriniere_crd_to_markdown_crd_to_markdown_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "crd-to-markdown_Darwin_x86_64",
            "crd-to-markdown_Linux_arm64",
            "crd-to-markdown_Linux_i386",
            "crd-to-markdown_Linux_x86_64",
            "crd-to-markdown_Windows_i386.exe",
            "crd-to-markdown_Windows_x86_64.exe",
        ]
    }

    #[test]
    fn test_clamoriniere_crd_to_markdown_crd_to_markdown_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 1),
            ],
            &clamoriniere_crd_to_markdown_crd_to_markdown_names(),
            "crd-to-markdown",
        );
    }

    fn cli_cli_gh_names() -> Vec<&'static str> {
        vec![
            "gh_2.87.3_checksums.txt",
            "gh_2.87.3_linux_386.deb",
            "gh_2.87.3_linux_386.rpm",
            "gh_2.87.3_linux_386.tar.gz",
            "gh_2.87.3_linux_amd64.deb",
            "gh_2.87.3_linux_amd64.rpm",
            "gh_2.87.3_linux_amd64.tar.gz",
            "gh_2.87.3_linux_arm64.deb",
            "gh_2.87.3_linux_arm64.rpm",
            "gh_2.87.3_linux_arm64.tar.gz",
            "gh_2.87.3_linux_armv6.deb",
            "gh_2.87.3_linux_armv6.rpm",
            "gh_2.87.3_linux_armv6.tar.gz",
            "gh_2.87.3_macOS_amd64.zip",
            "gh_2.87.3_macOS_arm64.zip",
            "gh_2.87.3_macOS_universal.pkg",
            "gh_2.87.3_windows_386.msi",
            "gh_2.87.3_windows_386.zip",
            "gh_2.87.3_windows_amd64.msi",
            "gh_2.87.3_windows_amd64.zip",
            "gh_2.87.3_windows_arm64.msi",
            "gh_2.87.3_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_cli_cli_gh_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 3),
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 9),
                (Platform::Osx64, 13),
                (Platform::OsxArm64, 14),
                (Platform::Win32, 17),
                (Platform::Win64, 19),
                (Platform::WinArm64, 21),
            ],
            &cli_cli_gh_names(),
            "gh",
        );
    }

    fn clog_tool_clog_cli_clog_names() -> Vec<&'static str> {
        vec![
            "clog-v0.9.3-aarch64-unknown-linux-gnu.tar.gz",
            "clog-v0.9.3-armv7-unknown-linux-gnueabihf.tar.gz",
            "clog-v0.9.3-i686-apple-darwin.tar.gz",
            "clog-v0.9.3-i686-unknown-linux-gnu.tar.gz",
            "clog-v0.9.3-i686-unknown-linux-musl.tar.gz",
            "clog-v0.9.3-x86_64-apple-darwin.tar.gz",
            "clog-v0.9.3-x86_64-unknown-linux-gnu.tar.gz",
            "clog-v0.9.3-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_clog_tool_clog_cli_clog_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 5),
            ],
            &clog_tool_clog_cli_clog_names(),
            "clog",
        );
    }

    fn cloudflare_cfssl_cfssl_bundle_names() -> Vec<&'static str> {
        vec![
            "cfssl-bundle_1.6.5_darwin_amd64",
            "cfssl-bundle_1.6.5_linux_amd64",
            "cfssl-bundle_1.6.5_linux_arm64",
            "cfssl-bundle_1.6.5_linux_armv6",
            "cfssl-bundle_1.6.5_linux_s390x",
            "cfssl-bundle_1.6.5_windows_amd64.exe",
            "cfssl-certinfo_1.6.5_darwin_amd64",
            "cfssl-certinfo_1.6.5_linux_amd64",
            "cfssl-certinfo_1.6.5_linux_arm64",
            "cfssl-certinfo_1.6.5_linux_armv6",
            "cfssl-certinfo_1.6.5_linux_s390x",
            "cfssl-certinfo_1.6.5_windows_amd64.exe",
            "cfssl-newkey_1.6.5_darwin_amd64",
            "cfssl-newkey_1.6.5_linux_amd64",
            "cfssl-newkey_1.6.5_linux_arm64",
            "cfssl-newkey_1.6.5_linux_armv6",
            "cfssl-newkey_1.6.5_linux_s390x",
            "cfssl-newkey_1.6.5_windows_amd64.exe",
            "cfssl-scan_1.6.5_darwin_amd64",
            "cfssl-scan_1.6.5_linux_amd64",
            "cfssl-scan_1.6.5_linux_arm64",
            "cfssl-scan_1.6.5_linux_armv6",
            "cfssl-scan_1.6.5_linux_s390x",
            "cfssl-scan_1.6.5_windows_amd64.exe",
            "cfssljson_1.6.5_darwin_amd64",
            "cfssljson_1.6.5_linux_amd64",
            "cfssljson_1.6.5_linux_arm64",
            "cfssljson_1.6.5_linux_armv6",
            "cfssljson_1.6.5_linux_s390x",
            "cfssljson_1.6.5_windows_amd64.exe",
            "cfssl_1.6.5_checksums.txt",
            "cfssl_1.6.5_darwin_amd64",
            "cfssl_1.6.5_darwin_arm64",
            "cfssl_1.6.5_linux_amd64",
            "cfssl_1.6.5_linux_arm64",
            "cfssl_1.6.5_linux_armv6",
            "cfssl_1.6.5_linux_s390x",
            "cfssl_1.6.5_windows_amd64.exe",
            "mkbundle_1.6.5_darwin_amd64",
            "mkbundle_1.6.5_linux_amd64",
            "mkbundle_1.6.5_linux_arm64",
            "mkbundle_1.6.5_linux_armv6",
            "mkbundle_1.6.5_linux_s390x",
            "mkbundle_1.6.5_windows_amd64.exe",
            "multirootca_1.6.5_darwin_amd64",
            "multirootca_1.6.5_linux_amd64",
            "multirootca_1.6.5_linux_arm64",
            "multirootca_1.6.5_linux_armv6",
            "multirootca_1.6.5_linux_s390x",
            "multirootca_1.6.5_windows_amd64.exe",
        ]
    }

    #[test]
    fn test_cloudflare_cfssl_cfssl_bundle_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 0),
            ],
            &cloudflare_cfssl_cfssl_bundle_names(),
            "cfssl-bundle",
        );
    }

    fn clowdhaus_eksup_eksup_names() -> Vec<&'static str> {
        vec![
            "eksup-v0.13.0-aarch64-apple-darwin.tar.gz",
            "eksup-v0.13.0-aarch64-unknown-linux-gnu.tar.gz",
            "eksup-v0.13.0-x86_64-apple-darwin.tar.gz",
            "eksup-v0.13.0-x86_64-pc-windows-msvc.zip",
            "eksup-v0.13.0-x86_64-unknown-linux-gnu.tar.gz",
        ]
    }

    #[test]
    fn test_clowdhaus_eksup_eksup_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 0),
            ],
            &clowdhaus_eksup_eksup_names(),
            "eksup",
        );
    }

    fn cococonscious_koji_koji_names() -> Vec<&'static str> {
        vec![
            "koji-aarch64-apple-darwin.tar.gz",
            "koji-aarch64-pc-windows-msvc.tar.gz",
            "koji-aarch64-pc-windows-msvc.zip",
            "koji-aarch64-unknown-linux-gnu.tar.gz",
            "koji-aarch64-unknown-linux-musl.tar.gz",
            "koji-x86_64-apple-darwin.tar.gz",
            "koji-x86_64-pc-windows-msvc.tar.gz",
            "koji-x86_64-pc-windows-msvc.zip",
            "koji-x86_64-unknown-linux-gnu.tar.gz",
            "koji-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_cococonscious_koji_koji_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 6),
                (Platform::WinArm64, 1),
            ],
            &cococonscious_koji_koji_names(),
            "koji",
        );
    }

    fn cocogitto_cocogitto_cocogitto_names() -> Vec<&'static str> {
        vec![
            "cocogitto-6.5.0-aarch64-apple-darwin.tar.gz",
            "cocogitto-6.5.0-aarch64-unknown-linux-gnu.tar.gz",
            "cocogitto-6.5.0-armv7-unknown-linux-musleabihf.tar.gz",
            "cocogitto-6.5.0-x86_64-apple-darwin.tar.gz",
            "cocogitto-6.5.0-x86_64-pc-windows-msvc.tar.gz",
            "cocogitto-6.5.0-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_cocogitto_cocogitto_cocogitto_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 3),
                (Platform::Win64, 4),
            ],
            &cocogitto_cocogitto_cocogitto_names(),
            "cocogitto",
        );
    }

    fn containers_fuse_overlayfs_fuse_overlayfs_names() -> Vec<&'static str> {
        vec![
            "fuse-overlayfs-aarch64",
            "fuse-overlayfs-armv7l",
            "fuse-overlayfs-ppc64le",
            "fuse-overlayfs-riscv64",
            "fuse-overlayfs-s390x",
            "fuse-overlayfs-x86_64",
            "SHA256SUMS",
            "SOURCE_DATE_EPOCH",
        ]
    }

    #[test]
    fn test_containers_fuse_overlayfs_fuse_overlayfs_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 0),
            ],
            &containers_fuse_overlayfs_fuse_overlayfs_names(),
            "fuse-overlayfs",
        );
    }

    fn containrrr_shoutrrr_shoutrrr_names() -> Vec<&'static str> {
        vec![
            "shoutrrr_0.8.0_checksums.txt",
            "shoutrrr_linux_386.tar.gz",
            "shoutrrr_linux_amd64.tar.gz",
            "shoutrrr_linux_arm64.tar.gz",
            "shoutrrr_linux_armv6.tar.gz",
            "shoutrrr_windows_386.zip",
            "shoutrrr_windows_amd64.zip",
            "shoutrrr_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_containrrr_shoutrrr_shoutrrr_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 1),
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 3),
                (Platform::Win32, 5),
                (Platform::Win64, 6),
                (Platform::WinArm64, 7),
            ],
            &containrrr_shoutrrr_shoutrrr_names(),
            "shoutrrr",
        );
    }

    fn cpisciotta_xcbeautify_xcbeautify_names() -> Vec<&'static str> {
        vec![
            "xcbeautify-3.1.4-arm64-apple-macosx.zip",
            "xcbeautify-3.1.4-universal-apple-macosx.zip",
            "xcbeautify-3.1.4-x86_64-apple-macosx.zip",
            "xcbeautify-3.1.4-x86_64-unknown-linux-gnu.tar.xz",
        ]
    }

    #[test]
    fn test_cpisciotta_xcbeautify_xcbeautify_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 0),
            ],
            &cpisciotta_xcbeautify_xcbeautify_names(),
            "xcbeautify",
        );
    }

    fn crazywhalecc_static_php_cli_spc_names() -> Vec<&'static str> {
        vec![
            "spc-linux-aarch64.tar.gz",
            "spc-linux-x86_64.tar.gz",
            "spc-macos-aarch64.tar.gz",
            "spc-macos-x86_64.tar.gz",
            "spc-windows-x64.exe",
        ]
    }

    #[test]
    fn test_crazywhalecc_static_php_cli_spc_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 2),
            ],
            &crazywhalecc_static_php_cli_spc_names(),
            "spc",
        );
    }

    fn cross_rs_cross_cross_names() -> Vec<&'static str> {
        vec![
            "cross-x86_64-apple-darwin.tar.gz",
            "cross-x86_64-pc-windows-msvc.tar.gz",
            "cross-x86_64-unknown-linux-gnu.tar.gz",
            "cross-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_cross_rs_cross_cross_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::Osx64, 0),
                (Platform::Win64, 1),
            ],
            &cross_rs_cross_cross_names(),
            "cross",
        );
    }

    fn cswank_kcli_kcli_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "kcli_1.8.3_Darwin_i386.tar.gz",
            "kcli_1.8.3_Darwin_x86_64.tar.gz",
            "kcli_1.8.3_Linux_i386.tar.gz",
            "kcli_1.8.3_Linux_x86_64.tar.gz",
            "kcli_1.8.3_Windows_i386.tar.gz",
            "kcli_1.8.3_Windows_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_cswank_kcli_kcli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::Osx64, 2),
                (Platform::Win64, 6),
            ],
            &cswank_kcli_kcli_names(),
            "kcli",
        );
    }

    fn cyberark_kubeletctl_kubeletctl_names() -> Vec<&'static str> {
        vec![
            "kubeletctl_darwin_amd64",
            "kubeletctl_darwin_arm64",
            "kubeletctl_linux_386",
            "kubeletctl_linux_amd64",
            "kubeletctl_linux_arm64",
            "kubeletctl_windows_386.exe",
            "kubeletctl_windows_amd64.exe",
        ]
    }

    #[test]
    fn test_cyberark_kubeletctl_kubeletctl_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 2),
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 1),
            ],
            &cyberark_kubeletctl_kubeletctl_names(),
            "kubeletctl",
        );
    }

    fn dag_andersen_argocd_diff_preview_argocd_diff_preview_names() -> Vec<&'static str> {
        vec![
            "argocd-diff-preview-Darwin-aarch64.tar.gz",
            "argocd-diff-preview-Darwin-x86_64.tar.gz",
            "argocd-diff-preview-Linux-aarch64.tar.gz",
            "argocd-diff-preview-Linux-x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_dag_andersen_argocd_diff_preview_argocd_diff_preview_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
            ],
            &dag_andersen_argocd_diff_preview_argocd_diff_preview_names(),
            "argocd-diff-preview",
        );
    }

    fn dalance_procs_procs_names() -> Vec<&'static str> {
        vec![
            "procs-0.14.11-1.x86_64.rpm",
            "procs-v0.14.11-aarch64-linux.zip",
            "procs-v0.14.11-aarch64-mac.zip",
            "procs-v0.14.11-x86_64-linux.zip",
            "procs-v0.14.11-x86_64-mac.zip",
            "procs-v0.14.11-x86_64-windows.zip",
        ]
    }

    #[test]
    fn test_dalance_procs_procs_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::Osx64, 4),
                (Platform::Win64, 5),
            ],
            &dalance_procs_procs_names(),
            "procs",
        );
    }

    fn dandavison_delta_delta_names() -> Vec<&'static str> {
        vec![
            "delta-0.18.2-aarch64-apple-darwin.tar.gz",
            "delta-0.18.2-aarch64-unknown-linux-gnu.tar.gz",
            "delta-0.18.2-arm-unknown-linux-gnueabihf.tar.gz",
            "delta-0.18.2-i686-unknown-linux-gnu.tar.gz",
            "delta-0.18.2-x86_64-apple-darwin.tar.gz",
            "delta-0.18.2-x86_64-pc-windows-msvc.zip",
            "delta-0.18.2-x86_64-unknown-linux-gnu.tar.gz",
            "delta-0.18.2-x86_64-unknown-linux-musl.tar.gz",
            "git-delta-musl_0.18.2_amd64.deb",
            "git-delta_0.18.2_amd64.deb",
            "git-delta_0.18.2_arm64.deb",
            "git-delta_0.18.2_armhf.deb",
            "git-delta_0.18.2_i386.deb",
        ]
    }

    #[test]
    fn test_dandavison_delta_delta_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 0),
            ],
            &dandavison_delta_delta_names(),
            "delta",
        );
    }

    fn danvergara_dblab_dblab_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "dblab_0.34.3_darwin_amd64.tar.gz",
            "dblab_0.34.3_darwin_arm64.tar.gz",
            "dblab_0.34.3_linux_amd64.tar.gz",
            "dblab_0.34.3_linux_arm64.tar.gz",
            "dblab_0.34.3_windows_amd64.tar.gz",
        ]
    }

    #[test]
    fn test_danvergara_dblab_dblab_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 5),
            ],
            &danvergara_dblab_dblab_names(),
            "dblab",
        );
    }

    fn databricks_click_click_names() -> Vec<&'static str> {
        vec![
            "click-v0.6.3-arm-unknown-linux-gnueabihf.tar.gz",
            "click-v0.6.3-x86_64-apple-darwin.tar.gz",
            "click-v0.6.3-x86_64-pc-windows-gnu.zip",
            "click-v0.6.3-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_databricks_click_click_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::Osx64, 1),
                (Platform::Win64, 2),
            ],
            &databricks_click_click_names(),
            "click",
        );
    }

    fn datanymizer_datanymizer_pg_datanymizer_names() -> Vec<&'static str> {
        vec![
            "pg_datanymizer-alpine-x86_64.tar.gz",
            "pg_datanymizer-darwin-x86_64.tar.gz",
            "pg_datanymizer-linux-x86_64.tar.gz",
            "pg_datanymizer-win-x64.zip",
        ]
    }

    #[test]
    fn test_datanymizer_datanymizer_pg_datanymizer_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 1),
                (Platform::Win64, 3),
            ],
            &datanymizer_datanymizer_pg_datanymizer_names(),
            "pg_datanymizer",
        );
    }

    fn datreeio_datree_datree_cli_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "datree-cli_1.9.19_Darwin_arm64.zip",
            "datree-cli_1.9.19_Darwin_x86_64.zip",
            "datree-cli_1.9.19_Linux_386.zip",
            "datree-cli_1.9.19_Linux_arm64.zip",
            "datree-cli_1.9.19_Linux_x86_64.zip",
            "datree-cli_1.9.19_windows_386.zip",
            "datree-cli_1.9.19_windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_datreeio_datree_datree_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 3),
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win32, 6),
                (Platform::Win64, 7),
            ],
            &datreeio_datree_datree_cli_names(),
            "datree-cli",
        );
    }

    fn ddev_ddev_ddev_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "ddev-wsl2_1.25.1_linux_amd64.deb",
            "ddev-wsl2_1.25.1_linux_amd64.rpm",
            "ddev-wsl2_1.25.1_linux_arm64.deb",
            "ddev-wsl2_1.25.1_linux_arm64.rpm",
            "ddev_1.25.1_linux_amd64.deb",
            "ddev_1.25.1_linux_amd64.rpm",
            "ddev_1.25.1_linux_arm64.deb",
            "ddev_1.25.1_linux_arm64.rpm",
            "ddev_linux-amd64.v1.25.1.tar.gz",
            "ddev_linux-arm64.v1.25.1.tar.gz",
            "ddev_macos-amd64.v1.25.1.tar.gz",
            "ddev_macos-arm64.v1.25.1.tar.gz",
            "ddev_shell_completion_scripts.v1.25.1.tar.gz",
            "ddev_windows-amd64.v1.25.1.zip",
            "ddev_windows-arm64.v1.25.1.zip",
            "ddev_windows_amd64_installer.v1.25.1.exe",
            "ddev_windows_arm64_installer.v1.25.1.exe",
        ]
    }

    #[test]
    fn test_ddev_ddev_ddev_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 10),
                (Platform::Osx64, 11),
                (Platform::OsxArm64, 12),
                (Platform::Win64, 14),
            ],
            &ddev_ddev_ddev_names(),
            "ddev",
        );
    }

    fn dduan_tre_tre_names() -> Vec<&'static str> {
        vec![
            "tre-v0.4.0-aarch64-apple-darwin.tar.gz",
            "tre-v0.4.0-arm-unknown-linux-gnueabihf.tar.gz",
            "tre-v0.4.0-i686-pc-windows-msvc.zip",
            "tre-v0.4.0-x86_64-apple-darwin.tar.gz",
            "tre-v0.4.0-x86_64-pc-windows-gnu.zip",
            "tre-v0.4.0-x86_64-pc-windows-msvc.zip",
            "tre-v0.4.0-x86_64-unknown-linux-musl.tar.gz",
            "tre-v0.4.1-x86_64-pc-windows-msvc.msi",
        ]
    }

    #[test]
    fn test_dduan_tre_tre_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 5),
            ],
            &dduan_tre_tre_names(),
            "tre",
        );
    }

    fn defenseunicorns_uds_cli_uds_cli_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "sbom_uds-cli_v0.28.3_Darwin_all.sbom",
            "sbom_uds-cli_v0.28.3_Darwin_amd64.sbom",
            "sbom_uds-cli_v0.28.3_Darwin_arm64.sbom",
            "sbom_uds-cli_v0.28.3_Linux_amd64.sbom",
            "sbom_uds-cli_v0.28.3_Linux_arm64.sbom",
            "uds-cli_v0.28.3_Darwin_all",
            "uds-cli_v0.28.3_Darwin_amd64",
            "uds-cli_v0.28.3_Darwin_arm64",
            "uds-cli_v0.28.3_Linux_amd64",
            "uds-cli_v0.28.3_Linux_arm64",
        ]
    }

    #[test]
    fn test_defenseunicorns_uds_cli_uds_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 10),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 8),
            ],
            &defenseunicorns_uds_cli_uds_cli_names(),
            "uds-cli",
        );
    }

    fn denisidoro_navi_navi_names() -> Vec<&'static str> {
        vec![
            "navi-v2.24.0-aarch64-linux-android.tar.gz",
            "navi-v2.24.0-aarch64-unknown-linux-gnu.tar.gz",
            "navi-v2.24.0-armv7-linux-androideabi.tar.gz",
            "navi-v2.24.0-armv7-unknown-linux-musleabihf.tar.gz",
            "navi-v2.24.0-x86_64-pc-windows-gnu.zip",
            "navi-v2.24.0-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_denisidoro_navi_navi_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 1),
                (Platform::Win64, 4),
            ],
            &denisidoro_navi_navi_names(),
            "navi",
        );
    }

    fn derailed_k9s_k9s_names() -> Vec<&'static str> {
        vec![
            "checksums.sha256",
            "k9s_Darwin_amd64.tar.gz",
            "k9s_Darwin_amd64.tar.gz.sbom.json",
            "k9s_Darwin_arm64.tar.gz",
            "k9s_Darwin_arm64.tar.gz.sbom.json",
            "k9s_Freebsd_amd64.tar.gz",
            "k9s_Freebsd_amd64.tar.gz.sbom.json",
            "k9s_Freebsd_arm64.tar.gz",
            "k9s_Freebsd_arm64.tar.gz.sbom.json",
            "k9s_linux_amd64.apk",
            "k9s_linux_amd64.deb",
            "k9s_linux_amd64.rpm",
            "k9s_Linux_amd64.tar.gz",
            "k9s_Linux_amd64.tar.gz.sbom.json",
            "k9s_linux_arm.apk",
            "k9s_linux_arm.deb",
            "k9s_linux_arm.rpm",
            "k9s_linux_arm64.apk",
            "k9s_linux_arm64.deb",
            "k9s_linux_arm64.rpm",
            "k9s_Linux_arm64.tar.gz",
            "k9s_Linux_arm64.tar.gz.sbom.json",
            "k9s_Linux_armv7.tar.gz",
            "k9s_Linux_armv7.tar.gz.sbom.json",
            "k9s_linux_ppc64le.apk",
            "k9s_linux_ppc64le.deb",
            "k9s_linux_ppc64le.rpm",
            "k9s_Linux_ppc64le.tar.gz",
            "k9s_Linux_ppc64le.tar.gz.sbom.json",
            "k9s_linux_s390x.apk",
            "k9s_linux_s390x.deb",
            "k9s_linux_s390x.rpm",
            "k9s_Linux_s390x.tar.gz",
            "k9s_Linux_s390x.tar.gz.sbom.json",
            "k9s_Windows_amd64.zip",
            "k9s_Windows_amd64.zip.sbom.json",
            "k9s_Windows_arm64.zip",
            "k9s_Windows_arm64.zip.sbom.json",
        ]
    }

    #[test]
    fn test_derailed_k9s_k9s_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 12),
                (Platform::LinuxAarch64, 20),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 34),
                (Platform::WinArm64, 36),
            ],
            &derailed_k9s_k9s_names(),
            "k9s",
        );
    }

    fn derailed_popeye_popeye_names() -> Vec<&'static str> {
        vec![
            "checksums.sha256",
            "popeye_darwin_amd64.tar.gz",
            "popeye_darwin_amd64.tar.gz.sbom.json",
            "popeye_darwin_arm64.tar.gz",
            "popeye_darwin_arm64.tar.gz.sbom.json",
            "popeye_freebsd_amd64.tar.gz",
            "popeye_freebsd_amd64.tar.gz.sbom.json",
            "popeye_freebsd_arm64.tar.gz",
            "popeye_freebsd_arm64.tar.gz.sbom.json",
            "popeye_linux_amd64.apk",
            "popeye_linux_amd64.deb",
            "popeye_linux_amd64.rpm",
            "popeye_linux_amd64.tar.gz",
            "popeye_linux_amd64.tar.gz.sbom.json",
            "popeye_linux_arm64.apk",
            "popeye_linux_arm64.deb",
            "popeye_linux_arm64.rpm",
            "popeye_linux_arm64.tar.gz",
            "popeye_linux_arm64.tar.gz.sbom.json",
            "popeye_linux_ppc64le.apk",
            "popeye_linux_ppc64le.deb",
            "popeye_linux_ppc64le.rpm",
            "popeye_linux_ppc64le.tar.gz",
            "popeye_linux_ppc64le.tar.gz.sbom.json",
            "popeye_linux_s390x.apk",
            "popeye_linux_s390x.deb",
            "popeye_linux_s390x.rpm",
            "popeye_linux_s390x.tar.gz",
            "popeye_linux_s390x.tar.gz.sbom.json",
            "popeye_windows_amd64.tar.gz",
            "popeye_windows_amd64.tar.gz.sbom.json",
            "popeye_windows_arm64.tar.gz",
            "popeye_windows_arm64.tar.gz.sbom.json",
        ]
    }

    #[test]
    fn test_derailed_popeye_popeye_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 12),
                (Platform::LinuxAarch64, 17),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 29),
                (Platform::WinArm64, 31),
            ],
            &derailed_popeye_popeye_names(),
            "popeye",
        );
    }

    fn devops_works_dw_query_digest_dw_query_digest_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "dw-query-digest_0.9.6_darwin_amd64.tar.gz",
            "dw-query-digest_0.9.6_linux_386.tar.gz",
            "dw-query-digest_0.9.6_linux_amd64.tar.gz",
            "dw-query-digest_0.9.6_windows_386.tar.gz",
            "dw-query-digest_0.9.6_windows_amd64.tar.gz",
        ]
    }

    #[test]
    fn test_devops_works_dw_query_digest_dw_query_digest_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::Osx64, 1),
                (Platform::Win32, 4),
                (Platform::Win64, 5),
            ],
            &devops_works_dw_query_digest_dw_query_digest_names(),
            "dw-query-digest",
        );
    }

    fn dhall_lang_dhall_haskell_dhall_names() -> Vec<&'static str> {
        vec![
            "dhall-1.42.2-aarch64-darwin.tar.bz2",
            "dhall-1.42.2-x86_64-darwin.tar.bz2",
            "dhall-1.42.2-x86_64-linux.tar.bz2",
            "dhall-1.42.2-x86_64-windows.zip",
            "dhall-bash-1.0.41-aarch64-darwin.tar.bz2",
            "dhall-bash-1.0.41-x86_64-darwin.tar.bz2",
            "dhall-bash-1.0.41-x86_64-linux.tar.bz2",
            "dhall-bash-1.0.41-x86_64-windows.zip",
            "dhall-csv-1.0.4-aarch64-darwin.tar.bz2",
            "dhall-csv-1.0.4-x86_64-darwin.tar.bz2",
            "dhall-csv-1.0.4-x86_64-linux.tar.bz2",
            "dhall-csv-1.0.4-x86_64-windows.zip",
            "dhall-docs-1.0.12-aarch64-darwin.tar.bz2",
            "dhall-docs-1.0.12-x86_64-darwin.tar.bz2",
            "dhall-docs-1.0.12-x86_64-linux.tar.bz2",
            "dhall-docs-1.0.12-x86_64-windows.zip",
            "dhall-json-1.7.12-aarch64-darwin.tar.bz2",
            "dhall-json-1.7.12-x86_64-darwin.tar.bz2",
            "dhall-json-1.7.12-x86_64-linux.tar.bz2",
            "dhall-json-1.7.12-x86_64-windows.zip",
            "dhall-lsp-server-1.1.4-aarch64-darwin.tar.bz2",
            "dhall-lsp-server-1.1.4-x86_64-darwin.tar.bz2",
            "dhall-lsp-server-1.1.4-x86_64-linux.tar.bz2",
            "dhall-lsp-server-1.1.4-x86_64-windows.zip",
            "dhall-nix-1.1.27-aarch64-darwin.tar.bz2",
            "dhall-nix-1.1.27-x86_64-darwin.tar.bz2",
            "dhall-nix-1.1.27-x86_64-linux.tar.bz2",
            "dhall-openapi-1.0.7-aarch64-darwin.tar.bz2",
            "dhall-openapi-1.0.7-x86_64-darwin.tar.bz2",
            "dhall-openapi-1.0.7-x86_64-linux.tar.bz2",
            "dhall-openapi-1.0.7-x86_64-windows.zip",
            "dhall-toml-1.0.4-aarch64-darwin.tar.bz2",
            "dhall-toml-1.0.4-x86_64-darwin.tar.bz2",
            "dhall-toml-1.0.4-x86_64-linux.tar.bz2",
            "dhall-toml-1.0.4-x86_64-windows.zip",
            "dhall-yaml-1.2.12-aarch64-darwin.tar.bz2",
            "dhall-yaml-1.2.12-x86_64-darwin.tar.bz2",
            "dhall-yaml-1.2.12-x86_64-linux.tar.bz2",
            "dhall-yaml-1.2.12-x86_64-windows.zip",
        ]
    }

    #[test]
    fn test_dhall_lang_dhall_haskell_dhall_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 3),
            ],
            &dhall_lang_dhall_haskell_dhall_names(),
            "dhall",
        );
    }

    fn dhth_bmm_bmm_names() -> Vec<&'static str> {
        vec![
            "bmm-aarch64-apple-darwin.tar.xz",
            "bmm-aarch64-apple-darwin.tar.xz.sha256",
            "bmm-installer.sh",
            "bmm-x86_64-apple-darwin.tar.xz",
            "bmm-x86_64-apple-darwin.tar.xz.sha256",
            "bmm-x86_64-unknown-linux-gnu.tar.xz",
            "bmm-x86_64-unknown-linux-gnu.tar.xz.sha256",
            "bmm-x86_64-unknown-linux-musl.tar.xz",
            "bmm-x86_64-unknown-linux-musl.tar.xz.sha256",
            "bmm.rb",
            "dist-manifest.json",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_dhth_bmm_bmm_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 0),
            ],
            &dhth_bmm_bmm_names(),
            "bmm",
        );
    }

    fn dimo414_bkt_bkt_v_names() -> Vec<&'static str> {
        vec![
            "bkt.v0.8.2.aarch64-apple-darwin.zip",
            "bkt.v0.8.2.aarch64-unknown-linux-gnu.zip",
            "bkt.v0.8.2.arm-unknown-linux-gnueabihf.zip",
            "bkt.v0.8.2.i686-unknown-linux-gnu.zip",
            "bkt.v0.8.2.i686-unknown-linux-musl.zip",
            "bkt.v0.8.2.x86_64-apple-darwin.zip",
            "bkt.v0.8.2.x86_64-pc-windows-msvc.zip",
            "bkt.v0.8.2.x86_64-unknown-linux-gnu.zip",
            "bkt.v0.8.2.x86_64-unknown-linux-musl.zip",
        ]
    }

    #[test]
    fn test_dimo414_bkt_bkt_v_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 5),
                (Platform::Win64, 6),
            ],
            &dimo414_bkt_bkt_v_names(),
            "bkt.v",
        );
    }

    fn dlvhdr_gh_dash_gh_dash_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "gh-dash_v4.22.0_android-arm64",
            "gh-dash_v4.22.0_darwin-amd64",
            "gh-dash_v4.22.0_darwin-arm64",
            "gh-dash_v4.22.0_freebsd-386",
            "gh-dash_v4.22.0_freebsd-amd64",
            "gh-dash_v4.22.0_freebsd-arm64",
            "gh-dash_v4.22.0_freebsd-arm_6",
            "gh-dash_v4.22.0_freebsd-arm_7",
            "gh-dash_v4.22.0_linux-386",
            "gh-dash_v4.22.0_linux-amd64",
            "gh-dash_v4.22.0_linux-arm64",
            "gh-dash_v4.22.0_linux-arm_6",
            "gh-dash_v4.22.0_linux-arm_7",
            "gh-dash_v4.22.0_windows-386.exe",
            "gh-dash_v4.22.0_windows-amd64.exe",
            "gh-dash_v4.22.0_windows-arm64.exe",
            "gh-dash_v4.22.0_windows-arm_6.exe",
            "gh-dash_v4.22.0_windows-arm_7.exe",
        ]
    }

    #[test]
    fn test_dlvhdr_gh_dash_gh_dash_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 9),
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 11),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 3),
            ],
            &dlvhdr_gh_dash_gh_dash_names(),
            "gh-dash",
        );
    }

    fn dmtrkovalenko_blendr_blendr_names() -> Vec<&'static str> {
        vec![
            "blendr-aarch64-apple-darwin.tar.gz",
            "blendr-aarch64-unknown-linux-gnu.tar.gz",
            "blendr-arm-unknown-linux-gnueabihf.tar.gz",
            "blendr-arm-unknown-linux-musleabihf.tar.gz",
            "blendr-i686-pc-windows-msvc.tar.gz",
            "blendr-i686-unknown-linux-gnu.tar.gz",
            "blendr-i686-unknown-linux-musl.tar.gz",
            "blendr-x86_64-apple-darwin.tar.gz",
            "blendr-x86_64-pc-windows-gnu.tar.gz",
            "blendr-x86_64-pc-windows-msvc.tar.gz",
            "blendr-x86_64-unknown-linux-gnu.tar.gz",
            "blendr-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_dmtrkovalenko_blendr_blendr_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 11),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 9),
            ],
            &dmtrkovalenko_blendr_blendr_names(),
            "blendr",
        );
    }

    fn docker_compose_docker_compose_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "docker-compose-darwin-aarch64",
            "docker-compose-darwin-aarch64.provenance.json",
            "docker-compose-darwin-aarch64.sbom.json",
            "docker-compose-darwin-aarch64.sha256",
            "docker-compose-darwin-aarch64.sigstore.json",
            "docker-compose-darwin-x86_64",
            "docker-compose-darwin-x86_64.provenance.json",
            "docker-compose-darwin-x86_64.sbom.json",
            "docker-compose-darwin-x86_64.sha256",
            "docker-compose-darwin-x86_64.sigstore.json",
            "docker-compose-linux-aarch64",
            "docker-compose-linux-aarch64.provenance.json",
            "docker-compose-linux-aarch64.sbom.json",
            "docker-compose-linux-aarch64.sha256",
            "docker-compose-linux-aarch64.sigstore.json",
            "docker-compose-linux-armv6",
            "docker-compose-linux-armv6.provenance.json",
            "docker-compose-linux-armv6.sbom.json",
            "docker-compose-linux-armv6.sha256",
            "docker-compose-linux-armv6.sigstore.json",
            "docker-compose-linux-armv7",
            "docker-compose-linux-armv7.provenance.json",
            "docker-compose-linux-armv7.sbom.json",
            "docker-compose-linux-armv7.sha256",
            "docker-compose-linux-armv7.sigstore.json",
            "docker-compose-linux-ppc64le",
            "docker-compose-linux-ppc64le.provenance.json",
            "docker-compose-linux-ppc64le.sbom.json",
            "docker-compose-linux-ppc64le.sha256",
            "docker-compose-linux-ppc64le.sigstore.json",
            "docker-compose-linux-riscv64",
            "docker-compose-linux-riscv64.provenance.json",
            "docker-compose-linux-riscv64.sbom.json",
            "docker-compose-linux-riscv64.sha256",
            "docker-compose-linux-riscv64.sigstore.json",
            "docker-compose-linux-s390x",
            "docker-compose-linux-s390x.provenance.json",
            "docker-compose-linux-s390x.sbom.json",
            "docker-compose-linux-s390x.sha256",
            "docker-compose-linux-s390x.sigstore.json",
            "docker-compose-linux-x86_64",
            "docker-compose-linux-x86_64.provenance.json",
            "docker-compose-linux-x86_64.sbom.json",
            "docker-compose-linux-x86_64.sha256",
            "docker-compose-linux-x86_64.sigstore.json",
            "docker-compose-windows-aarch64.exe",
            "docker-compose-windows-aarch64.exe.sha256",
            "docker-compose-windows-aarch64.provenance.json",
            "docker-compose-windows-aarch64.sbom.json",
            "docker-compose-windows-aarch64.sigstore.json",
            "docker-compose-windows-x86_64.exe",
            "docker-compose-windows-x86_64.exe.sha256",
            "docker-compose-windows-x86_64.provenance.json",
            "docker-compose-windows-x86_64.sbom.json",
            "docker-compose-windows-x86_64.sigstore.json",
        ]
    }

    #[test]
    fn test_docker_compose_docker_compose_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 41),
                (Platform::LinuxAarch64, 11),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 1),
            ],
            &docker_compose_docker_compose_names(),
            "docker-compose",
        );
    }

    fn domoritz_arrow_tools_csv2arrow_names() -> Vec<&'static str> {
        vec![
            "csv2arrow-aarch64-apple-darwin.tar.xz",
            "csv2arrow-aarch64-apple-darwin.tar.xz.sha256",
            "csv2arrow-aarch64-unknown-linux-gnu.tar.xz",
            "csv2arrow-aarch64-unknown-linux-gnu.tar.xz.sha256",
            "csv2arrow-installer.sh",
            "csv2arrow-x86_64-unknown-linux-gnu.tar.xz",
            "csv2arrow-x86_64-unknown-linux-gnu.tar.xz.sha256",
            "csv2arrow.rb",
            "csv2parquet-aarch64-apple-darwin.tar.xz",
            "csv2parquet-aarch64-apple-darwin.tar.xz.sha256",
            "csv2parquet-aarch64-unknown-linux-gnu.tar.xz",
            "csv2parquet-aarch64-unknown-linux-gnu.tar.xz.sha256",
            "csv2parquet-installer.sh",
            "csv2parquet-x86_64-unknown-linux-gnu.tar.xz",
            "csv2parquet-x86_64-unknown-linux-gnu.tar.xz.sha256",
            "csv2parquet.rb",
            "dist-manifest.json",
            "json2arrow-aarch64-apple-darwin.tar.xz",
            "json2arrow-aarch64-apple-darwin.tar.xz.sha256",
            "json2arrow-aarch64-unknown-linux-gnu.tar.xz",
            "json2arrow-aarch64-unknown-linux-gnu.tar.xz.sha256",
            "json2arrow-installer.sh",
            "json2arrow-x86_64-unknown-linux-gnu.tar.xz",
            "json2arrow-x86_64-unknown-linux-gnu.tar.xz.sha256",
            "json2arrow.rb",
            "json2parquet-aarch64-apple-darwin.tar.xz",
            "json2parquet-aarch64-apple-darwin.tar.xz.sha256",
            "json2parquet-aarch64-unknown-linux-gnu.tar.xz",
            "json2parquet-aarch64-unknown-linux-gnu.tar.xz.sha256",
            "json2parquet-installer.sh",
            "json2parquet-x86_64-unknown-linux-gnu.tar.xz",
            "json2parquet-x86_64-unknown-linux-gnu.tar.xz.sha256",
            "json2parquet.rb",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_domoritz_arrow_tools_csv2arrow_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 2),
                (Platform::OsxArm64, 0),
            ],
            &domoritz_arrow_tools_csv2arrow_names(),
            "csv2arrow",
        );
    }

    fn dotenv_linter_dotenv_linter_dotenv_linter_names() -> Vec<&'static str> {
        vec![
            "dotenv-linter-alpine-aarch64.tar.gz",
            "dotenv-linter-alpine-x86_64.tar.gz",
            "dotenv-linter-darwin-arm64.tar.gz",
            "dotenv-linter-darwin-x86_64.tar.gz",
            "dotenv-linter-linux-aarch64.tar.gz",
            "dotenv-linter-linux-x86_64.tar.gz",
            "dotenv-linter-win-aarch64.zip",
            "dotenv-linter-win-x64.zip",
        ]
    }

    #[test]
    fn test_dotenv_linter_dotenv_linter_dotenv_linter_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 2),
            ],
            &dotenv_linter_dotenv_linter_dotenv_linter_names(),
            "dotenv-linter",
        );
    }

    fn dotenvx_dotenvx_dotenvx_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "dotenvx-1.52.0-darwin-amd64.tar.gz",
            "dotenvx-1.52.0-darwin-arm64.tar.gz",
            "dotenvx-1.52.0-darwin-x86_64.tar.gz",
            "dotenvx-1.52.0-linux-aarch64.tar.gz",
            "dotenvx-1.52.0-linux-amd64.tar.gz",
            "dotenvx-1.52.0-linux-arm64.tar.gz",
            "dotenvx-1.52.0-linux-armv7l.tar.gz",
            "dotenvx-1.52.0-linux-x86_64.tar.gz",
            "dotenvx-1.52.0-windows-amd64.tar.gz",
            "dotenvx-1.52.0-windows-amd64.zip",
            "dotenvx-1.52.0-windows-x86_64.tar.gz",
            "dotenvx-1.52.0-windows-x86_64.zip",
            "dotenvx-darwin-amd64.tar.gz",
            "dotenvx-darwin-arm64.tar.gz",
            "dotenvx-darwin-x86_64.tar.gz",
            "dotenvx-linux-amd64.tar.gz",
            "dotenvx-linux-arm64.tar.gz",
            "dotenvx-linux-armv7l.tar.gz",
            "dotenvx-linux-x86_64.tar.gz",
            "dotenvx-windows-amd64.tar.gz",
            "dotenvx-windows-amd64.zip",
            "dotenvx-windows-x86_64.tar.gz",
            "dotenvx-windows-x86_64.zip",
            "install.sh",
        ]
    }

    #[test]
    fn test_dotenvx_dotenvx_dotenvx_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 9),
            ],
            &dotenvx_dotenvx_dotenvx_names(),
            "dotenvx",
        );
    }

    fn dprint_dprint_dprint_names() -> Vec<&'static str> {
        vec![
            "dprint-aarch64-apple-darwin.zip",
            "dprint-aarch64-unknown-linux-gnu.zip",
            "dprint-aarch64-unknown-linux-musl.zip",
            "dprint-loongarch64-unknown-linux-gnu.zip",
            "dprint-loongarch64-unknown-linux-musl.zip",
            "dprint-riscv64gc-unknown-linux-gnu.zip",
            "dprint-x86_64-apple-darwin.zip",
            "dprint-x86_64-pc-windows-msvc-installer.exe",
            "dprint-x86_64-pc-windows-msvc.zip",
            "dprint-x86_64-unknown-linux-gnu.zip",
            "dprint-x86_64-unknown-linux-musl.zip",
            "SHASUMS256.txt",
        ]
    }

    #[test]
    fn test_dprint_dprint_dprint_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 8),
            ],
            &dprint_dprint_dprint_names(),
            "dprint",
        );
    }

    fn drager_wasm_pack_wasm_pack_names() -> Vec<&'static str> {
        vec![
            "wasm-pack-init.exe",
            "wasm-pack-v0.14.0-aarch64-apple-darwin.tar.gz",
            "wasm-pack-v0.14.0-aarch64-unknown-linux-musl.tar.gz",
            "wasm-pack-v0.14.0-x86_64-apple-darwin.tar.gz",
            "wasm-pack-v0.14.0-x86_64-pc-windows-msvc.tar.gz",
            "wasm-pack-v0.14.0-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_drager_wasm_pack_wasm_pack_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 3),
                (Platform::Win64, 4),
            ],
            &drager_wasm_pack_wasm_pack_names(),
            "wasm-pack",
        );
    }

    fn drlau_akashi_akashi_names() -> Vec<&'static str> {
        vec![
            "akashi_0.0.18_checksums.txt",
            "akashi_0.0.18_Darwin_arm64.tar.gz",
            "akashi_0.0.18_Darwin_x86_64.tar.gz",
            "akashi_0.0.18_Linux_arm64.tar.gz",
            "akashi_0.0.18_Linux_i386.tar.gz",
            "akashi_0.0.18_Linux_x86_64.tar.gz",
            "akashi_0.0.18_Windows_arm64.tar.gz",
            "akashi_0.0.18_Windows_i386.tar.gz",
            "akashi_0.0.18_Windows_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_drlau_akashi_akashi_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 8),
            ],
            &drlau_akashi_akashi_names(),
            "akashi",
        );
    }

    fn dtan4_s3url_s3url_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "s3url_Darwin_i386.tar.gz",
            "s3url_Darwin_x86_64.tar.gz",
            "s3url_Linux_arm64.tar.gz",
            "s3url_Linux_armv6.tar.gz",
            "s3url_Linux_i386.tar.gz",
            "s3url_Linux_x86_64.tar.gz",
            "s3url_Windows_i386.zip",
            "s3url_Windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_dtan4_s3url_s3url_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::Win64, 8),
            ],
            &dtan4_s3url_s3url_names(),
            "s3url",
        );
    }

    fn duckdb_duckdb_duckdb_cli_names() -> Vec<&'static str> {
        vec![
            "duckdb_cli-linux-amd64.gz",
            "duckdb_cli-linux-amd64.zip",
            "duckdb_cli-linux-arm64.gz",
            "duckdb_cli-linux-arm64.zip",
            "duckdb_cli-osx-amd64.gz",
            "duckdb_cli-osx-amd64.zip",
            "duckdb_cli-osx-arm64.gz",
            "duckdb_cli-osx-arm64.zip",
            "duckdb_cli-osx-universal.gz",
            "duckdb_cli-osx-universal.zip",
            "duckdb_cli-windows-amd64.zip",
            "duckdb_cli-windows-arm64.zip",
            "libduckdb-linux-amd64.zip",
            "libduckdb-linux-arm64.zip",
            "libduckdb-osx-universal.zip",
            "libduckdb-src.zip",
            "libduckdb-windows-amd64.zip",
            "libduckdb-windows-arm64.zip",
            "static-libs-linux-amd64.zip",
            "static-libs-linux-arm64.zip",
            "static-libs-osx-amd64.zip",
            "static-libs-osx-arm64.zip",
            "static-libs-windows-mingw.zip",
        ]
    }

    #[test]
    fn test_duckdb_duckdb_duckdb_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 9),
                (Platform::Win64, 10),
                (Platform::WinArm64, 11),
            ],
            &duckdb_duckdb_duckdb_cli_names(),
            "duckdb_cli",
        );
    }

    fn dutchcoders_cloudman_cloudman_names() -> Vec<&'static str> {
        vec![
            "cloudman-0.1.7-x86_64-apple-darwin.tar.gz",
            "cloudman-0.1.7-x86_64-unknown-linux-gnu.tar.gz",
        ]
    }

    #[test]
    fn test_dutchcoders_cloudman_cloudman_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 0),
            ],
            &dutchcoders_cloudman_cloudman_names(),
            "cloudman",
        );
    }

    fn dwisiswant0_tlder_tlder_names() -> Vec<&'static str> {
        vec![
            "tlder_0.1.1_checksums.txt",
            "tlder_v0.1.1-darwin_arm64",
            "tlder_v0.1.1-darwin_x86_64",
            "tlder_v0.1.1-linux_arm",
            "tlder_v0.1.1-linux_arm64",
            "tlder_v0.1.1-linux_i386",
            "tlder_v0.1.1-linux_x86_64",
            "tlder_v0.1.1-windows_arm.exe",
            "tlder_v0.1.1-windows_arm64.exe",
            "tlder_v0.1.1-windows_i386.exe",
            "tlder_v0.1.1-windows_x86_64.exe",
        ]
    }

    #[test]
    fn test_dwisiswant0_tlder_tlder_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
            ],
            &dwisiswant0_tlder_tlder_names(),
            "tlder",
        );
    }

    fn dyne_slangroom_exec_slangroom_exec_names() -> Vec<&'static str> {
        vec![
            "slangroom-exec-Darwin-arm64",
            "slangroom-exec-Darwin-arm64.tar.gz",
            "slangroom-exec-Darwin-x86_64",
            "slangroom-exec-Darwin-x86_64.tar.gz",
            "slangroom-exec-Linux-aarch64",
            "slangroom-exec-Linux-aarch64.tar.gz",
            "slangroom-exec-Linux-arm64",
            "slangroom-exec-Linux-arm64.tar.gz",
            "slangroom-exec-Linux-x86_64",
            "slangroom-exec-Linux-x86_64.tar.gz",
            "slangroom-exec-Windows-x86_64.exe",
            "slangroom-exec-Windows-x86_64.tar.gz",
            "slexfe",
        ]
    }

    #[test]
    fn test_dyne_slangroom_exec_slangroom_exec_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 7),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 1),
            ],
            &dyne_slangroom_exec_slangroom_exec_names(),
            "slangroom-exec",
        );
    }

    fn dyne_zenroom_zenroom_names() -> Vec<&'static str> {
        vec![
            "lua-exec",
            "zencode-exec",
            "zencode-exec.exe",
            "zenroom",
            "zenroom-android.aar",
            "zenroom-arm_64-linux.zip",
            "zenroom-arm_hf-linux.zip",
            "zenroom-ios.zip",
            "zenroom-linux.zip",
            "zenroom-osx.zip",
            "zenroom-win64.zip",
            "zenroom-x86_64-linux.zip",
            "zenroom.exe",
        ]
    }

    #[test]
    fn test_dyne_zenroom_zenroom_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::OsxArm64, 9),
                (Platform::Win32, 10),
                (Platform::Win64, 10),
                (Platform::WinArm64, 10),
            ],
            &dyne_zenroom_zenroom_names(),
            "zenroom",
        );
    }

    fn edoardottt_depsdev_depsdev_names() -> Vec<&'static str> {
        vec![
            "depsdev_0.2.1_checksums.txt",
            "depsdev_0.2.1_linux_386.zip",
            "depsdev_0.2.1_linux_amd64.zip",
            "depsdev_0.2.1_linux_arm.zip",
            "depsdev_0.2.1_linux_arm64.zip",
            "depsdev_0.2.1_macOS_amd64.zip",
            "depsdev_0.2.1_macOS_arm64.zip",
            "depsdev_0.2.1_windows_386.zip",
            "depsdev_0.2.1_windows_amd64.zip",
        ]
    }

    #[test]
    fn test_edoardottt_depsdev_depsdev_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 1),
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 6),
                (Platform::Win32, 7),
                (Platform::Win64, 8),
            ],
            &edoardottt_depsdev_depsdev_names(),
            "depsdev",
        );
    }

    fn errata_ai_vale_vale_names() -> Vec<&'static str> {
        vec![
            "vale_3.13.1_checksums.txt",
            "vale_3.13.1_Linux_64-bit.tar.gz",
            "vale_3.13.1_Linux_arm64.tar.gz",
            "vale_3.13.1_macOS_64-bit.tar.gz",
            "vale_3.13.1_macOS_arm64.tar.gz",
            "vale_3.13.1_Windows_64-bit.zip",
        ]
    }

    #[test]
    fn test_errata_ai_vale_vale_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 5),
            ],
            &errata_ai_vale_vale_names(),
            "vale",
        );
    }

    fn evilmartians_lefthook_lefthook_names() -> Vec<&'static str> {
        vec![
            "lefthook_2.1.2_amd64.apk",
            "lefthook_2.1.2_amd64.deb",
            "lefthook_2.1.2_amd64.rpm",
            "lefthook_2.1.2_arm64.apk",
            "lefthook_2.1.2_arm64.deb",
            "lefthook_2.1.2_arm64.rpm",
            "lefthook_2.1.2_Freebsd_arm64",
            "lefthook_2.1.2_Freebsd_arm64.gz",
            "lefthook_2.1.2_Freebsd_x86_64",
            "lefthook_2.1.2_Freebsd_x86_64.gz",
            "lefthook_2.1.2_Linux_aarch64",
            "lefthook_2.1.2_Linux_aarch64.gz",
            "lefthook_2.1.2_Linux_arm64",
            "lefthook_2.1.2_Linux_arm64.gz",
            "lefthook_2.1.2_Linux_x86_64",
            "lefthook_2.1.2_Linux_x86_64.gz",
            "lefthook_2.1.2_MacOS_arm64",
            "lefthook_2.1.2_MacOS_arm64.gz",
            "lefthook_2.1.2_MacOS_x86_64",
            "lefthook_2.1.2_MacOS_x86_64.gz",
            "lefthook_2.1.2_Openbsd_arm64",
            "lefthook_2.1.2_Openbsd_arm64.gz",
            "lefthook_2.1.2_Openbsd_x86_64",
            "lefthook_2.1.2_Openbsd_x86_64.gz",
            "lefthook_2.1.2_Windows_arm64.exe",
            "lefthook_2.1.2_Windows_arm64.gz",
            "lefthook_2.1.2_Windows_i386.exe",
            "lefthook_2.1.2_Windows_i386.gz",
            "lefthook_2.1.2_Windows_x86_64.exe",
            "lefthook_2.1.2_Windows_x86_64.gz",
            "lefthook_checksums.txt",
        ]
    }

    #[test]
    fn test_evilmartians_lefthook_lefthook_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 15),
                (Platform::LinuxAarch64, 11),
                (Platform::Osx64, 19),
                (Platform::OsxArm64, 17),
                (Platform::Win64, 29),
                (Platform::WinArm64, 25),
            ],
            &evilmartians_lefthook_lefthook_names(),
            "lefthook",
        );
    }

    fn eza_community_eza_eza_names() -> Vec<&'static str> {
        vec![
            "completions-0.23.4.tar.gz",
            "eza.exe_x86_64-pc-windows-gnu.tar.gz",
            "eza.exe_x86_64-pc-windows-gnu.zip",
            "eza_aarch64-unknown-linux-gnu.tar.gz",
            "eza_aarch64-unknown-linux-gnu.zip",
            "eza_aarch64-unknown-linux-gnu_no_libgit.tar.gz",
            "eza_aarch64-unknown-linux-gnu_no_libgit.zip",
            "eza_arm-unknown-linux-gnueabihf.tar.gz",
            "eza_arm-unknown-linux-gnueabihf.zip",
            "eza_arm-unknown-linux-gnueabihf_no_libgit.tar.gz",
            "eza_arm-unknown-linux-gnueabihf_no_libgit.zip",
            "eza_x86_64-unknown-linux-gnu.tar.gz",
            "eza_x86_64-unknown-linux-gnu.zip",
            "eza_x86_64-unknown-linux-musl.tar.gz",
            "eza_x86_64-unknown-linux-musl.zip",
            "man-0.23.4.tar.gz",
        ]
    }

    #[test]
    fn test_eza_community_eza_eza_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 13),
                (Platform::LinuxAarch64, 3),
                (Platform::Win64, 1),
            ],
            &eza_community_eza_eza_names(),
            "eza",
        );
    }

    fn fastfetch_cli_fastfetch_fastfetch_names() -> Vec<&'static str> {
        vec![
            "fastfetch-dragonfly-amd64.tar.gz",
            "fastfetch-dragonfly-amd64.zip",
            "fastfetch-freebsd-amd64.pkg",
            "fastfetch-freebsd-amd64.tar.gz",
            "fastfetch-freebsd-amd64.zip",
            "fastfetch-haiku-amd64.tar.gz",
            "fastfetch-haiku-amd64.zip",
            "fastfetch-linux-aarch64-polyfilled.deb",
            "fastfetch-linux-aarch64-polyfilled.rpm",
            "fastfetch-linux-aarch64-polyfilled.tar.gz",
            "fastfetch-linux-aarch64-polyfilled.zip",
            "fastfetch-linux-aarch64.deb",
            "fastfetch-linux-aarch64.rpm",
            "fastfetch-linux-aarch64.tar.gz",
            "fastfetch-linux-aarch64.zip",
            "fastfetch-linux-amd64-polyfilled.deb",
            "fastfetch-linux-amd64-polyfilled.rpm",
            "fastfetch-linux-amd64-polyfilled.tar.gz",
            "fastfetch-linux-amd64-polyfilled.zip",
            "fastfetch-linux-amd64.deb",
            "fastfetch-linux-amd64.rpm",
            "fastfetch-linux-amd64.tar.gz",
            "fastfetch-linux-amd64.zip",
            "fastfetch-linux-armv6l.deb",
            "fastfetch-linux-armv6l.rpm",
            "fastfetch-linux-armv6l.tar.gz",
            "fastfetch-linux-armv6l.zip",
            "fastfetch-linux-armv7l.deb",
            "fastfetch-linux-armv7l.rpm",
            "fastfetch-linux-armv7l.tar.gz",
            "fastfetch-linux-armv7l.zip",
            "fastfetch-linux-i686.deb",
            "fastfetch-linux-i686.rpm",
            "fastfetch-linux-i686.tar.gz",
            "fastfetch-linux-i686.zip",
            "fastfetch-linux-ppc64le.deb",
            "fastfetch-linux-ppc64le.rpm",
            "fastfetch-linux-ppc64le.tar.gz",
            "fastfetch-linux-ppc64le.zip",
            "fastfetch-linux-riscv64.deb",
            "fastfetch-linux-riscv64.rpm",
            "fastfetch-linux-riscv64.tar.gz",
            "fastfetch-linux-riscv64.zip",
            "fastfetch-linux-s390x.deb",
            "fastfetch-linux-s390x.rpm",
            "fastfetch-linux-s390x.tar.gz",
            "fastfetch-linux-s390x.zip",
            "fastfetch-macos-aarch64.tar.gz",
            "fastfetch-macos-aarch64.zip",
            "fastfetch-macos-amd64.tar.gz",
            "fastfetch-macos-amd64.zip",
            "fastfetch-musl-amd64.tar.gz",
            "fastfetch-musl-amd64.zip",
            "fastfetch-netbsd-amd64.tar.gz",
            "fastfetch-netbsd-amd64.zip",
            "fastfetch-omnios-amd64.tar.gz",
            "fastfetch-omnios-amd64.zip",
            "fastfetch-openbsd-amd64.tar.gz",
            "fastfetch-openbsd-amd64.zip",
            "fastfetch-solaris-amd64.tar.gz",
            "fastfetch-solaris-amd64.zip",
            "fastfetch-windows-aarch64.7z",
            "fastfetch-windows-aarch64.zip",
            "fastfetch-windows-amd64-win7.7z",
            "fastfetch-windows-amd64-win7.zip",
            "fastfetch-windows-amd64.7z",
            "fastfetch-windows-amd64.zip",
        ]
    }

    #[test]
    fn test_fastfetch_cli_fastfetch_fastfetch_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 21),
                (Platform::LinuxAarch64, 13),
                (Platform::Osx64, 49),
                (Platform::OsxArm64, 47),
                (Platform::Win64, 66),
                (Platform::WinArm64, 62),
            ],
            &fastfetch_cli_fastfetch_fastfetch_names(),
            "fastfetch",
        );
    }

    fn fastly_terrctl_terrctl_names() -> Vec<&'static str> {
        vec![
            "terrctl-android_arm64-1.0.2.zip",
            "terrctl-dragonflybsd_amd64-1.0.2.tar.gz",
            "terrctl-freebsd_amd64-1.0.2.tar.gz",
            "terrctl-freebsd_arm-1.0.2.tar.gz",
            "terrctl-freebsd_i386-1.0.2.tar.gz",
            "terrctl-linux_arm-1.0.2.tar.gz",
            "terrctl-linux_arm64-1.0.2.tar.gz",
            "terrctl-linux_i386-1.0.2.tar.gz",
            "terrctl-linux_mips-1.0.2.tar.gz",
            "terrctl-linux_mips64-1.0.2.tar.gz",
            "terrctl-linux_mips64le-1.0.2.tar.gz",
            "terrctl-linux_mipsle-1.0.2.tar.gz",
            "terrctl-linux_x86_64-1.0.2.tar.gz",
            "terrctl-macos-1.0.2.tar.gz",
            "terrctl-macos_arm64-1.0.2.tar.gz",
            "terrctl-netbsd_amd64-1.0.2.tar.gz",
            "terrctl-netbsd_i386-1.0.2.tar.gz",
            "terrctl-openbsd_amd64-1.0.2.tar.gz",
            "terrctl-openbsd_i386-1.0.2.tar.gz",
            "terrctl-win32-1.0.2.zip",
            "terrctl-win64-1.0.2.zip",
        ]
    }

    #[test]
    fn test_fastly_terrctl_terrctl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 12),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 13),
                (Platform::OsxArm64, 14),
                (Platform::Win64, 20),
            ],
            &fastly_terrctl_terrctl_names(),
            "terrctl",
        );
    }

    fn ffuf_ffuf_ffuf_names() -> Vec<&'static str> {
        vec![
            "ffuf_2.1.0_checksums.txt",
            "ffuf_2.1.0_checksums.txt.sig",
            "ffuf_2.1.0_freebsd_386.tar.gz",
            "ffuf_2.1.0_freebsd_amd64.tar.gz",
            "ffuf_2.1.0_freebsd_armv6.tar.gz",
            "ffuf_2.1.0_linux_386.tar.gz",
            "ffuf_2.1.0_linux_amd64.tar.gz",
            "ffuf_2.1.0_linux_arm64.tar.gz",
            "ffuf_2.1.0_linux_armv6.tar.gz",
            "ffuf_2.1.0_macOS_amd64.tar.gz",
            "ffuf_2.1.0_macOS_arm64.tar.gz",
            "ffuf_2.1.0_openbsd_386.tar.gz",
            "ffuf_2.1.0_openbsd_amd64.tar.gz",
            "ffuf_2.1.0_openbsd_arm64.tar.gz",
            "ffuf_2.1.0_openbsd_armv6.tar.gz",
            "ffuf_2.1.0_windows_386.zip",
            "ffuf_2.1.0_windows_amd64.zip",
            "ffuf_2.1.0_windows_arm64.zip",
            "ffuf_2.1.0_windows_armv6.zip",
        ]
    }

    #[test]
    fn test_ffuf_ffuf_ffuf_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 5),
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 7),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 10),
                (Platform::Win32, 15),
                (Platform::Win64, 16),
                (Platform::WinArm64, 17),
            ],
            &ffuf_ffuf_ffuf_names(),
            "ffuf",
        );
    }

    fn firecracker_microvm_firecracker_firecracker_names() -> Vec<&'static str> {
        vec![
            "firecracker-v1.14.2-aarch64.tgz",
            "firecracker-v1.14.2-aarch64.tgz.sha256.txt",
            "firecracker-v1.14.2-x86_64.tgz",
            "firecracker-v1.14.2-x86_64.tgz.sha256.txt",
            "test_results.tar.gz",
        ]
    }

    #[test]
    fn test_firecracker_microvm_firecracker_firecracker_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 0),
            ],
            &firecracker_microvm_firecracker_firecracker_names(),
            "firecracker",
        );
    }

    fn fish_shell_fish_shell_fish_names() -> Vec<&'static str> {
        vec![
            "fish-4.5.0-linux-aarch64.tar.xz",
            "fish-4.5.0-linux-x86_64.tar.xz",
            "fish-4.5.0.app.zip",
            "fish-4.5.0.pkg",
            "fish-4.5.0.tar.xz",
            "fish-4.5.0.tar.xz.asc",
        ]
    }

    #[test]
    fn test_fish_shell_fish_shell_fish_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 3),
            ],
            &fish_shell_fish_shell_fish_names(),
            "fish",
        );
    }

    fn fission_fission_fission_names() -> Vec<&'static str> {
        vec![
            "builder_1.22.0_linux_amd64.sbom.json",
            "builder_1.22.0_linux_amd64.sbom.json.sig.bundle",
            "builder_1.22.0_linux_arm64.sbom.json",
            "builder_1.22.0_linux_arm64.sbom.json.sig.bundle",
            "checksums.txt",
            "checksums.txt.sig.bundle",
            "fetcher_1.22.0_linux_amd64.sbom.json",
            "fetcher_1.22.0_linux_amd64.sbom.json.sig.bundle",
            "fetcher_1.22.0_linux_arm64.sbom.json",
            "fetcher_1.22.0_linux_arm64.sbom.json.sig.bundle",
            "fission-all-v1.22.0-minikube.yaml",
            "fission-all-v1.22.0-openshift.yaml",
            "fission-all-v1.22.0.yaml",
            "fission-bundle_1.22.0_linux_amd64.sbom.json",
            "fission-bundle_1.22.0_linux_amd64.sbom.json.sig.bundle",
            "fission-bundle_1.22.0_linux_arm64.sbom.json",
            "fission-bundle_1.22.0_linux_arm64.sbom.json.sig.bundle",
            "fission-v1.22.0-darwin-amd64",
            "fission-v1.22.0-darwin-amd64.sig.bundle",
            "fission-v1.22.0-darwin-arm64",
            "fission-v1.22.0-darwin-arm64.sig.bundle",
            "fission-v1.22.0-linux-amd64",
            "fission-v1.22.0-linux-amd64.sig.bundle",
            "fission-v1.22.0-linux-arm64",
            "fission-v1.22.0-linux-arm64.sig.bundle",
            "fission-v1.22.0-windows-amd64.exe",
            "fission-v1.22.0-windows-amd64.exe.sig.bundle",
            "fission.exe_1.22.0_windows_amd64.sbom.json",
            "fission.exe_1.22.0_windows_amd64.sbom.json.sig.bundle",
            "fission_1.22.0_darwin_amd64.sbom.json",
            "fission_1.22.0_darwin_amd64.sbom.json.sig.bundle",
            "fission_1.22.0_darwin_arm64.sbom.json",
            "fission_1.22.0_darwin_arm64.sbom.json.sig.bundle",
            "fission_1.22.0_linux_amd64.sbom.json",
            "fission_1.22.0_linux_amd64.sbom.json.sig.bundle",
            "fission_1.22.0_linux_arm64.sbom.json",
            "fission_1.22.0_linux_arm64.sbom.json.sig.bundle",
            "pre-upgrade-checks_1.22.0_linux_amd64.sbom.json",
            "pre-upgrade-checks_1.22.0_linux_amd64.sbom.json.sig.bundle",
            "pre-upgrade-checks_1.22.0_linux_arm64.sbom.json",
            "pre-upgrade-checks_1.22.0_linux_arm64.sbom.json.sig.bundle",
            "reporter_1.22.0_linux_amd64.sbom.json",
            "reporter_1.22.0_linux_amd64.sbom.json.sig.bundle",
            "reporter_1.22.0_linux_arm64.sbom.json",
            "reporter_1.22.0_linux_arm64.sbom.json.sig.bundle",
        ]
    }

    #[test]
    fn test_fission_fission_fission_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 21),
                (Platform::LinuxAarch64, 23),
                (Platform::Osx64, 17),
                (Platform::OsxArm64, 19),
            ],
            &fission_fission_fission_names(),
            "fission",
        );
    }

    fn flatt_security_shisho_build_names() -> Vec<&'static str> {
        vec![
            "build-x86_64-apple-darwin.zip",
            "build-x86_64-pc-windows-gnu.zip",
            "build-x86_64-unknown-linux-gnu.zip",
        ]
    }

    #[test]
    fn test_flatt_security_shisho_build_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 0),
                (Platform::Win64, 1),
            ],
            &flatt_security_shisho_build_names(),
            "build",
        );
    }

    fn flosell_iam_policy_json_to_terraform_iam_policy_json_to_terraform_names() -> Vec<&'static str> {
        vec![
            "iam-policy-json-to-terraform.exe",
            "iam-policy-json-to-terraform_alpine",
            "iam-policy-json-to-terraform_amd64",
            "iam-policy-json-to-terraform_arm64",
            "iam-policy-json-to-terraform_darwin",
            "iam-policy-json-to-terraform_darwin_arm",
        ]
    }

    #[test]
    fn test_flosell_iam_policy_json_to_terraform_iam_policy_json_to_terraform_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 0),
            ],
            &flosell_iam_policy_json_to_terraform_iam_policy_json_to_terraform_names(),
            "iam-policy-json-to-terraform",
        );
    }

    fn foresterre_cargo_msrv_cargo_msrv_names() -> Vec<&'static str> {
        vec![
            "cargo-msrv-aarch64-apple-darwin-v0.19.1.tgz",
            "cargo-msrv-x86_64-apple-darwin-v0.19.1.tgz",
            "cargo-msrv-x86_64-pc-windows-msvc-v0.19.1.zip",
            "cargo-msrv-x86_64-unknown-linux-gnu-v0.19.1.tgz",
            "cargo-msrv-x86_64-unknown-linux-musl-v0.19.1.tgz",
        ]
    }

    #[test]
    fn test_foresterre_cargo_msrv_cargo_msrv_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 2),
            ],
            &foresterre_cargo_msrv_cargo_msrv_names(),
            "cargo-msrv",
        );
    }

    fn fortio_fortio_fortio_names() -> Vec<&'static str> {
        vec![
            "fortio-1.74.0-1.aarch64.rpm",
            "fortio-1.74.0-1.ppc64le.rpm",
            "fortio-1.74.0-1.s390x.rpm",
            "fortio-1.74.0-1.x86_64.rpm",
            "fortio-linux_amd64-1.74.0.tgz",
            "fortio-linux_arm64-1.74.0.tgz",
            "fortio-linux_ppc64le-1.74.0.tgz",
            "fortio-linux_s390x-1.74.0.tgz",
            "fortio_1.74.0.orig.tar.gz",
            "fortio_1.74.0_amd64.deb",
            "fortio_1.74.0_arm64.deb",
            "fortio_1.74.0_ppc64el.deb",
            "fortio_1.74.0_s390x.deb",
            "fortio_win_1.74.0.zip",
        ]
    }

    #[test]
    fn test_fortio_fortio_fortio_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 5),
                (Platform::Win64, 13),
            ],
            &fortio_fortio_fortio_names(),
            "fortio",
        );
    }

    fn foundry_rs_foundry_foundry_names() -> Vec<&'static str> {
        vec![
            "foundry_man_v1.6.0-rc1.tar.gz",
            "foundry_v1.6.0-rc1_alpine_amd64.attestation.txt",
            "foundry_v1.6.0-rc1_alpine_amd64.tar.gz",
            "foundry_v1.6.0-rc1_alpine_arm64.attestation.txt",
            "foundry_v1.6.0-rc1_alpine_arm64.tar.gz",
            "foundry_v1.6.0-rc1_darwin_amd64.attestation.txt",
            "foundry_v1.6.0-rc1_darwin_amd64.tar.gz",
            "foundry_v1.6.0-rc1_darwin_arm64.attestation.txt",
            "foundry_v1.6.0-rc1_darwin_arm64.tar.gz",
            "foundry_v1.6.0-rc1_linux_amd64.attestation.txt",
            "foundry_v1.6.0-rc1_linux_amd64.tar.gz",
            "foundry_v1.6.0-rc1_linux_arm64.attestation.txt",
            "foundry_v1.6.0-rc1_linux_arm64.tar.gz",
            "foundry_v1.6.0-rc1_win32_amd64.attestation.txt",
            "foundry_v1.6.0-rc1_win32_amd64.zip",
        ]
    }

    #[test]
    fn test_foundry_rs_foundry_foundry_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 12),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 8),
                (Platform::Win64, 14),
            ],
            &foundry_rs_foundry_foundry_names(),
            "foundry",
        );
    }

    fn fujiwara_tfstate_lookup_tfstate_lookup_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "tfstate-lookup_1.10.0_darwin_amd64.tar.gz",
            "tfstate-lookup_1.10.0_darwin_arm64.tar.gz",
            "tfstate-lookup_1.10.0_linux_amd64.tar.gz",
            "tfstate-lookup_1.10.0_linux_arm64.tar.gz",
        ]
    }

    #[test]
    fn test_fujiwara_tfstate_lookup_tfstate_lookup_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
            ],
            &fujiwara_tfstate_lookup_tfstate_lookup_names(),
            "tfstate-lookup",
        );
    }

    fn fujiwara_tncl_tncl_names() -> Vec<&'static str> {
        vec![
            "tncl-aarch64-linux-musl",
            "tncl-x86_64-linux-musl",
        ]
    }

    #[test]
    fn test_fujiwara_tncl_tncl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 0),
            ],
            &fujiwara_tncl_tncl_names(),
            "tncl",
        );
    }

    fn fullstorydev_grpcui_grpcui_names() -> Vec<&'static str> {
        vec![
            "grpcui_1.4.3_checksums.txt",
            "grpcui_1.4.3_linux_arm64.tar.gz",
            "grpcui_1.4.3_linux_x86_32.tar.gz",
            "grpcui_1.4.3_linux_x86_64.tar.gz",
            "grpcui_1.4.3_osx_arm64.tar.gz",
            "grpcui_1.4.3_osx_x86_64.tar.gz",
            "grpcui_1.4.3_windows_x86_32.zip",
            "grpcui_1.4.3_windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_fullstorydev_grpcui_grpcui_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 7),
            ],
            &fullstorydev_grpcui_grpcui_names(),
            "grpcui",
        );
    }

    fn funbiscuit_spacedisplay_rs_spacedisplay_names() -> Vec<&'static str> {
        vec![
            "spacedisplay-amd64_linux",
            "spacedisplay-amd64_linux.snap",
            "spacedisplay-macos",
            "spacedisplay-win64.exe",
        ]
    }

    #[test]
    fn test_funbiscuit_spacedisplay_rs_spacedisplay_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 2),
            ],
            &funbiscuit_spacedisplay_rs_spacedisplay_names(),
            "spacedisplay",
        );
    }

    fn g_plane_pnpm_shell_completion_pnpm_shell_completion_names() -> Vec<&'static str> {
        vec![
            "pnpm-shell-completion_aarch64-apple-darwin.tar.gz",
            "pnpm-shell-completion_aarch64-apple-darwin.zip",
            "pnpm-shell-completion_aarch64-unknown-linux-gnu.tar.gz",
            "pnpm-shell-completion_aarch64-unknown-linux-gnu.zip",
            "pnpm-shell-completion_pwsh_aarch64-apple-darwin.zip",
            "pnpm-shell-completion_pwsh_aarch64-unknown-linux-gnu.zip",
            "pnpm-shell-completion_pwsh_x86_64-apple-darwin.zip",
            "pnpm-shell-completion_pwsh_x86_64-pc-windows-gnu.zip",
            "pnpm-shell-completion_pwsh_x86_64-unknown-linux-gnu.zip",
            "pnpm-shell-completion_pwsh_x86_64-unknown-linux-musl.zip",
            "pnpm-shell-completion_x86_64-apple-darwin.tar.gz",
            "pnpm-shell-completion_x86_64-apple-darwin.zip",
            "pnpm-shell-completion_x86_64-unknown-linux-gnu.tar.gz",
            "pnpm-shell-completion_x86_64-unknown-linux-gnu.zip",
            "pnpm-shell-completion_x86_64-unknown-linux-musl.tar.gz",
            "pnpm-shell-completion_x86_64-unknown-linux-musl.zip",
        ]
    }

    #[test]
    fn test_g_plane_pnpm_shell_completion_pnpm_shell_completion_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 14),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 7),
            ],
            &g_plane_pnpm_shell_completion_pnpm_shell_completion_names(),
            "pnpm-shell-completion",
        );
    }

    fn gabeduke_kubectl_iexec_kubectl_iexec_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "kubectl-iexec_v1.19.15-alphav1_Darwin_arm64.tar.gz",
            "kubectl-iexec_v1.19.15-alphav1_Darwin_x86_64.tar.gz",
            "kubectl-iexec_v1.19.15-alphav1_Linux_arm64.tar.gz",
            "kubectl-iexec_v1.19.15-alphav1_Linux_x86_64.tar.gz",
            "kubectl-iexec_v1.19.15-alphav1_Windows_arm64.tar.gz",
            "kubectl-iexec_v1.19.15-alphav1_Windows_i386.tar.gz",
            "kubectl-iexec_v1.19.15-alphav1_Windows_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_gabeduke_kubectl_iexec_kubectl_iexec_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 7),
                (Platform::WinArm64, 5),
            ],
            &gabeduke_kubectl_iexec_kubectl_iexec_names(),
            "kubectl-iexec",
        );
    }

    fn gcla_termshark_termshark_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "checksums.txt.sig",
            "termshark_2.4.0_freebsd_x64.tar.gz",
            "termshark_2.4.0_linux_arm64.tar.gz",
            "termshark_2.4.0_linux_armv6.tar.gz",
            "termshark_2.4.0_linux_x64.tar.gz",
            "termshark_2.4.0_macOS_arm64.tar.gz",
            "termshark_2.4.0_macOS_x64.tar.gz",
            "termshark_2.4.0_netbsd_x64.tar.gz",
            "termshark_2.4.0_openbsd_x64.tar.gz",
            "termshark_2.4.0_windows_x64.zip",
        ]
    }

    #[test]
    fn test_gcla_termshark_termshark_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 6),
                (Platform::Win64, 10),
            ],
            &gcla_termshark_termshark_names(),
            "termshark",
        );
    }

    fn getgauge_gauge_gauge_names() -> Vec<&'static str> {
        vec![
            "gauge-1.6.26-darwin.arm64.zip",
            "gauge-1.6.26-darwin.x86_64.zip",
            "gauge-1.6.26-freebsd.x86.zip",
            "gauge-1.6.26-freebsd.x86_64.zip",
            "gauge-1.6.26-linux.arm64.zip",
            "gauge-1.6.26-linux.x86.zip",
            "gauge-1.6.26-linux.x86_64.zip",
            "gauge-1.6.26-windows.x86.exe",
            "gauge-1.6.26-windows.x86.zip",
            "gauge-1.6.26-windows.x86_64.exe",
            "gauge-1.6.26-windows.x86_64.zip",
        ]
    }

    #[test]
    fn test_getgauge_gauge_gauge_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 10),
            ],
            &getgauge_gauge_gauge_names(),
            "gauge",
        );
    }

    fn git_town_git_town_git_town_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "git-town_freebsd_arm_64.tar.gz",
            "git-town_freebsd_intel_64.tar.gz",
            "git-town_linux_arm_64.deb",
            "git-town_linux_arm_64.pkg.tar.zst",
            "git-town_linux_arm_64.rpm",
            "git-town_linux_arm_64.tar.gz",
            "git-town_linux_intel_64.deb",
            "git-town_linux_intel_64.pkg.tar.zst",
            "git-town_linux_intel_64.rpm",
            "git-town_linux_intel_64.tar.gz",
            "git-town_macos_arm_64.tar.gz",
            "git-town_macos_intel_64.tar.gz",
            "git-town_netbsd_intel_64.tar.gz",
            "git-town_windows_arm_64.zip",
            "git-town_windows_intel_64.msi",
            "git-town_windows_intel_64.zip",
        ]
    }

    #[test]
    fn test_git_town_git_town_git_town_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 12),
                (Platform::OsxArm64, 11),
                (Platform::Win64, 16),
                (Platform::WinArm64, 14),
            ],
            &git_town_git_town_git_town_names(),
            "git-town",
        );
    }

    fn github_copilot_cli_copilot_names() -> Vec<&'static str> {
        vec![
            "copilot-arm64.msi",
            "copilot-darwin-arm64.tar.gz",
            "copilot-darwin-x64.tar.gz",
            "copilot-linux-arm64.tar.gz",
            "copilot-linux-x64.tar.gz",
            "copilot-win32-arm64.zip",
            "copilot-win32-x64.zip",
            "copilot-x64.msi",
            "github-copilot-0.0.420.tgz",
            "SHA256SUMS.txt",
        ]
    }

    #[test]
    fn test_github_copilot_cli_copilot_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 6),
                (Platform::WinArm64, 5),
            ],
            &github_copilot_cli_copilot_names(),
            "copilot",
        );
    }

    fn github_copilot_language_server_release_copilot_language_server_names() -> Vec<&'static str> {
        vec![
            "copilot-language-server-darwin-arm64-1.439.0.zip",
            "copilot-language-server-darwin-x64-1.439.0.zip",
            "copilot-language-server-js-1.439.0.zip",
            "copilot-language-server-linux-arm64-1.439.0.zip",
            "copilot-language-server-linux-x64-1.439.0.zip",
            "copilot-language-server-native-1.439.0.zip",
            "copilot-language-server-win32-arm64-1.439.0.zip",
            "copilot-language-server-win32-x64-1.439.0.zip",
        ]
    }

    #[test]
    fn test_github_copilot_language_server_release_copilot_language_server_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 7),
                (Platform::WinArm64, 6),
            ],
            &github_copilot_language_server_release_copilot_language_server_names(),
            "copilot-language-server",
        );
    }

    fn github_gh_ost_gh_ost_binary_names() -> Vec<&'static str> {
        vec![
            "gh-ost",
            "gh-ost-1.1.7-1.x86_64.rpm",
            "gh-ost-binary-linux-amd64-20241219160321.tar.gz",
            "gh-ost-binary-linux-arm64-20241219160321.tar.gz",
            "gh-ost-binary-osx-amd64-20241219160321.tar.gz",
            "gh-ost-binary-osx-arm64-20241219160321.tar.gz",
            "gh-ost_1.1.7_amd64.deb",
        ]
    }

    #[test]
    fn test_github_gh_ost_gh_ost_binary_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 5),
            ],
            &github_gh_ost_gh_ost_binary_names(),
            "gh-ost-binary",
        );
    }

    fn gitleaks_gitleaks_gitleaks_names() -> Vec<&'static str> {
        vec![
            "gitleaks_8.30.0_checksums.txt",
            "gitleaks_8.30.0_darwin_arm64.tar.gz",
            "gitleaks_8.30.0_darwin_x64.tar.gz",
            "gitleaks_8.30.0_linux_arm64.tar.gz",
            "gitleaks_8.30.0_linux_armv6.tar.gz",
            "gitleaks_8.30.0_linux_armv7.tar.gz",
            "gitleaks_8.30.0_linux_x32.tar.gz",
            "gitleaks_8.30.0_linux_x64.tar.gz",
            "gitleaks_8.30.0_windows_armv6.zip",
            "gitleaks_8.30.0_windows_armv7.zip",
            "gitleaks_8.30.0_windows_x32.zip",
            "gitleaks_8.30.0_windows_x64.zip",
        ]
    }

    #[test]
    fn test_gitleaks_gitleaks_gitleaks_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 11),
            ],
            &gitleaks_gitleaks_gitleaks_names(),
            "gitleaks",
        );
    }

    fn gittools_gitversion_gitversion_names() -> Vec<&'static str> {
        vec![
            "gitversion-linux-arm64-6.6.0.tar.gz",
            "gitversion-linux-musl-arm64-6.6.0.tar.gz",
            "gitversion-linux-musl-x64-6.6.0.tar.gz",
            "gitversion-linux-x64-6.6.0.tar.gz",
            "gitversion-osx-arm64-6.6.0.tar.gz",
            "gitversion-osx-x64-6.6.0.tar.gz",
            "gitversion-win-arm64-6.6.0.zip",
            "gitversion-win-x64-6.6.0.zip",
        ]
    }

    #[test]
    fn test_gittools_gitversion_gitversion_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 7),
                (Platform::WinArm64, 6),
            ],
            &gittools_gitversion_gitversion_names(),
            "gitversion",
        );
    }

    fn gitui_org_gitui_gitui_names() -> Vec<&'static str> {
        vec![
            "gitui-linux-aarch64.tar.gz",
            "gitui-linux-arm.tar.gz",
            "gitui-linux-armv7.tar.gz",
            "gitui-linux-x86_64.tar.gz",
            "gitui-mac-x86.tar.gz",
            "gitui-mac.tar.gz",
            "gitui-win.msi",
            "gitui-win.tar.gz",
        ]
    }

    #[test]
    fn test_gitui_org_gitui_gitui_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 5),
                (Platform::Win32, 7),
                (Platform::Win64, 7),
                (Platform::WinArm64, 7),
            ],
            &gitui_org_gitui_gitui_names(),
            "gitui",
        );
    }

    fn go_task_task_task_names() -> Vec<&'static str> {
        vec![
            "task_3.48.0_linux_386.apk",
            "task_3.48.0_linux_386.deb",
            "task_3.48.0_linux_386.rpm",
            "task_3.48.0_linux_amd64.apk",
            "task_3.48.0_linux_amd64.deb",
            "task_3.48.0_linux_amd64.rpm",
            "task_3.48.0_linux_arm.apk",
            "task_3.48.0_linux_arm.deb",
            "task_3.48.0_linux_arm.rpm",
            "task_3.48.0_linux_arm64.apk",
            "task_3.48.0_linux_arm64.deb",
            "task_3.48.0_linux_arm64.rpm",
            "task_3.48.0_linux_riscv64.apk",
            "task_3.48.0_linux_riscv64.deb",
            "task_3.48.0_linux_riscv64.rpm",
            "task_checksums.txt",
            "task_darwin_amd64.tar.gz",
            "task_darwin_arm64.tar.gz",
            "task_freebsd_386.tar.gz",
            "task_freebsd_amd64.tar.gz",
            "task_freebsd_arm.tar.gz",
            "task_freebsd_arm64.tar.gz",
            "task_linux_386.tar.gz",
            "task_linux_amd64.tar.gz",
            "task_linux_arm.tar.gz",
            "task_linux_arm64.tar.gz",
            "task_linux_riscv64.tar.gz",
            "task_windows_386.zip",
            "task_windows_amd64.zip",
            "task_windows_arm.zip",
            "task_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_go_task_task_task_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 22),
                (Platform::Linux64, 23),
                (Platform::LinuxAarch64, 25),
                (Platform::Osx64, 16),
                (Platform::OsxArm64, 17),
                (Platform::Win32, 27),
                (Platform::Win64, 28),
                (Platform::WinArm64, 30),
            ],
            &go_task_task_task_names(),
            "task",
        );
    }

    fn goark_depm_depm_names() -> Vec<&'static str> {
        vec![
            "depm_0.6.6_checksums.txt",
            "depm_0.6.6_Darwin_64bit.tar.gz",
            "depm_0.6.6_Darwin_ARM64.tar.gz",
            "depm_0.6.6_FreeBSD_64bit.tar.gz",
            "depm_0.6.6_FreeBSD_ARM64.tar.gz",
            "depm_0.6.6_Linux_64bit.tar.gz",
            "depm_0.6.6_Linux_ARM64.tar.gz",
            "depm_0.6.6_Linux_RISCV.tar.gz",
            "depm_0.6.6_Windows_64bit.zip",
            "depm_0.6.6_Windows_ARM64.zip",
        ]
    }

    #[test]
    fn test_goark_depm_depm_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 8),
                (Platform::WinArm64, 9),
            ],
            &goark_depm_depm_names(),
            "depm",
        );
    }

    fn goark_gimei_cli_gimei_cli_names() -> Vec<&'static str> {
        vec![
            "gimei-cli_0.2.2_checksums.txt",
            "gimei-cli_0.2.2_FreeBSD_64bit.tar.gz",
            "gimei-cli_0.2.2_FreeBSD_ARM64.tar.gz",
            "gimei-cli_0.2.2_FreeBSD_ARMv6.tar.gz",
            "gimei-cli_0.2.2_Linux_64bit.tar.gz",
            "gimei-cli_0.2.2_Linux_ARM64.tar.gz",
            "gimei-cli_0.2.2_Linux_ARMv6.tar.gz",
            "gimei-cli_0.2.2_macOS_64bit.tar.gz",
            "gimei-cli_0.2.2_macOS_ARM64.tar.gz",
            "gimei-cli_0.2.2_Windows_64bit.zip",
            "gimei-cli_0.2.2_Windows_ARM64.zip",
            "gimei-cli_0.2.2_Windows_ARMv6.zip",
        ]
    }

    #[test]
    fn test_goark_gimei_cli_gimei_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 8),
                (Platform::Win64, 9),
                (Platform::WinArm64, 10),
            ],
            &goark_gimei_cli_gimei_cli_names(),
            "gimei-cli",
        );
    }

    fn goark_gnkf_gnkf_names() -> Vec<&'static str> {
        vec![
            "gnkf_0.7.9_checksums.txt",
            "gnkf_0.7.9_Darwin_64bit.tar.gz",
            "gnkf_0.7.9_Darwin_ARM64.tar.gz",
            "gnkf_0.7.9_FreeBSD_64bit.tar.gz",
            "gnkf_0.7.9_FreeBSD_ARM64.tar.gz",
            "gnkf_0.7.9_Linux_64bit.tar.gz",
            "gnkf_0.7.9_Linux_ARM64.tar.gz",
            "gnkf_0.7.9_Linux_RISCV.tar.gz",
            "gnkf_0.7.9_Windows_64bit.zip",
            "gnkf_0.7.9_Windows_ARM64.zip",
        ]
    }

    #[test]
    fn test_goark_gnkf_gnkf_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 6),
                (Platform::Win64, 8),
            ],
            &goark_gnkf_gnkf_names(),
            "gnkf",
        );
    }

    fn godotengine_godot_godot_v_names() -> Vec<&'static str> {
        vec![
            "godot-4.6.1-stable.tar.xz",
            "godot-4.6.1-stable.tar.xz.sha256",
            "godot-lib.4.6.1.stable.mono.template_release.aar",
            "godot-lib.4.6.1.stable.template_release.aar",
            "Godot_native_debug_symbols.4.6.1.stable.editor.android.zip",
            "Godot_native_debug_symbols.4.6.1.stable.template_release.android.zip",
            "Godot_v4.6.1-stable_android_editor.aab",
            "Godot_v4.6.1-stable_android_editor.apk",
            "Godot_v4.6.1-stable_android_editor_horizonos.apk",
            "Godot_v4.6.1-stable_android_editor_picoos.apk",
            "Godot_v4.6.1-stable_export_templates.tpz",
            "Godot_v4.6.1-stable_linux.arm32.zip",
            "Godot_v4.6.1-stable_linux.arm64.zip",
            "Godot_v4.6.1-stable_linux.x86_32.zip",
            "Godot_v4.6.1-stable_linux.x86_64.zip",
            "Godot_v4.6.1-stable_macos.universal.zip",
            "Godot_v4.6.1-stable_mono_export_templates.tpz",
            "Godot_v4.6.1-stable_mono_linux_arm32.zip",
            "Godot_v4.6.1-stable_mono_linux_arm64.zip",
            "Godot_v4.6.1-stable_mono_linux_x86_32.zip",
            "Godot_v4.6.1-stable_mono_linux_x86_64.zip",
            "Godot_v4.6.1-stable_mono_macos.universal.zip",
            "Godot_v4.6.1-stable_mono_win32.zip",
            "Godot_v4.6.1-stable_mono_win64.zip",
            "Godot_v4.6.1-stable_mono_windows_arm64.zip",
            "Godot_v4.6.1-stable_web_editor.zip",
            "Godot_v4.6.1-stable_win32.exe.zip",
            "Godot_v4.6.1-stable_win64.exe.zip",
            "Godot_v4.6.1-stable_windows_arm64.exe.zip",
            "SHA512-SUMS.txt",
        ]
    }

    #[test]
    fn test_godotengine_godot_godot_v_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 14),
                (Platform::LinuxAarch64, 12),
                (Platform::Osx64, 15),
                (Platform::OsxArm64, 15),
                (Platform::Win64, 27),
                (Platform::WinArm64, 28),
            ],
            &godotengine_godot_godot_v_names(),
            "Godot_v",
        );
    }

    fn gohugoio_hugo_hugo_extended_names() -> Vec<&'static str> {
        vec![
            "hugo_0.157.0_checksums.txt",
            "hugo_0.157.0_darwin-universal.pkg",
            "hugo_0.157.0_dragonfly-amd64.tar.gz",
            "hugo_0.157.0_freebsd-amd64.tar.gz",
            "hugo_0.157.0_Linux-64bit.tar.gz",
            "hugo_0.157.0_linux-amd64.deb",
            "hugo_0.157.0_linux-amd64.tar.gz",
            "hugo_0.157.0_linux-arm.tar.gz",
            "hugo_0.157.0_linux-arm64.deb",
            "hugo_0.157.0_linux-arm64.tar.gz",
            "hugo_0.157.0_netbsd-amd64.tar.gz",
            "hugo_0.157.0_openbsd-amd64.tar.gz",
            "hugo_0.157.0_solaris-amd64.tar.gz",
            "hugo_0.157.0_windows-amd64.zip",
            "hugo_0.157.0_windows-arm64.zip",
            "hugo_extended_0.157.0_darwin-universal.pkg",
            "hugo_extended_0.157.0_Linux-64bit.tar.gz",
            "hugo_extended_0.157.0_linux-amd64.deb",
            "hugo_extended_0.157.0_linux-amd64.tar.gz",
            "hugo_extended_0.157.0_linux-arm64.deb",
            "hugo_extended_0.157.0_linux-arm64.tar.gz",
            "hugo_extended_0.157.0_windows-amd64.zip",
            "hugo_extended_withdeploy_0.157.0_darwin-universal.pkg",
            "hugo_extended_withdeploy_0.157.0_Linux-64bit.tar.gz",
            "hugo_extended_withdeploy_0.157.0_linux-amd64.deb",
            "hugo_extended_withdeploy_0.157.0_linux-amd64.tar.gz",
            "hugo_extended_withdeploy_0.157.0_linux-arm64.deb",
            "hugo_extended_withdeploy_0.157.0_linux-arm64.tar.gz",
            "hugo_extended_withdeploy_0.157.0_windows-amd64.zip",
        ]
    }

    #[test]
    fn test_gohugoio_hugo_hugo_extended_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 18),
                (Platform::LinuxAarch64, 20),
                (Platform::Osx64, 15),
                (Platform::OsxArm64, 15),
                (Platform::Win64, 21),
            ],
            &gohugoio_hugo_hugo_extended_names(),
            "hugo_extended",
        );
    }

    fn gohugoio_hugo_hugo_names() -> Vec<&'static str> {
        vec![
            "hugo_0.157.0_checksums.txt",
            "hugo_0.157.0_darwin-universal.pkg",
            "hugo_0.157.0_dragonfly-amd64.tar.gz",
            "hugo_0.157.0_freebsd-amd64.tar.gz",
            "hugo_0.157.0_Linux-64bit.tar.gz",
            "hugo_0.157.0_linux-amd64.deb",
            "hugo_0.157.0_linux-amd64.tar.gz",
            "hugo_0.157.0_linux-arm.tar.gz",
            "hugo_0.157.0_linux-arm64.deb",
            "hugo_0.157.0_linux-arm64.tar.gz",
            "hugo_0.157.0_netbsd-amd64.tar.gz",
            "hugo_0.157.0_openbsd-amd64.tar.gz",
            "hugo_0.157.0_solaris-amd64.tar.gz",
            "hugo_0.157.0_windows-amd64.zip",
            "hugo_0.157.0_windows-arm64.zip",
            "hugo_extended_0.157.0_darwin-universal.pkg",
            "hugo_extended_0.157.0_Linux-64bit.tar.gz",
            "hugo_extended_0.157.0_linux-amd64.deb",
            "hugo_extended_0.157.0_linux-amd64.tar.gz",
            "hugo_extended_0.157.0_linux-arm64.deb",
            "hugo_extended_0.157.0_linux-arm64.tar.gz",
            "hugo_extended_0.157.0_windows-amd64.zip",
            "hugo_extended_withdeploy_0.157.0_darwin-universal.pkg",
            "hugo_extended_withdeploy_0.157.0_Linux-64bit.tar.gz",
            "hugo_extended_withdeploy_0.157.0_linux-amd64.deb",
            "hugo_extended_withdeploy_0.157.0_linux-amd64.tar.gz",
            "hugo_extended_withdeploy_0.157.0_linux-arm64.deb",
            "hugo_extended_withdeploy_0.157.0_linux-arm64.tar.gz",
            "hugo_extended_withdeploy_0.157.0_windows-amd64.zip",
        ]
    }

    #[test]
    fn test_gohugoio_hugo_hugo_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 9),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 13),
                (Platform::WinArm64, 14),
            ],
            &gohugoio_hugo_hugo_names(),
            "hugo",
        );
    }

    fn goodwithtech_dockle_dockle_names() -> Vec<&'static str> {
        vec![
            "dockle_0.4.15_checksums.txt",
            "dockle_0.4.15_Linux-386.apk",
            "dockle_0.4.15_Linux-386.deb",
            "dockle_0.4.15_Linux-386.rpm",
            "dockle_0.4.15_Linux-386.tar.gz",
            "dockle_0.4.15_Linux-64bit.apk",
            "dockle_0.4.15_Linux-64bit.deb",
            "dockle_0.4.15_Linux-64bit.rpm",
            "dockle_0.4.15_Linux-64bit.tar.gz",
            "dockle_0.4.15_Linux-ARM.apk",
            "dockle_0.4.15_Linux-ARM.deb",
            "dockle_0.4.15_Linux-ARM.rpm",
            "dockle_0.4.15_Linux-ARM.tar.gz",
            "dockle_0.4.15_Linux-ARM64.apk",
            "dockle_0.4.15_Linux-ARM64.deb",
            "dockle_0.4.15_Linux-ARM64.rpm",
            "dockle_0.4.15_Linux-ARM64.tar.gz",
            "dockle_0.4.15_Linux-loong64.apk",
            "dockle_0.4.15_Linux-loong64.deb",
            "dockle_0.4.15_Linux-loong64.rpm",
            "dockle_0.4.15_Linux-LOONG64.tar.gz",
            "dockle_0.4.15_macOS-64bit.tar.gz",
            "dockle_0.4.15_macOS-ARM64.tar.gz",
        ]
    }

    #[test]
    fn test_goodwithtech_dockle_dockle_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 4),
                (Platform::Linux64, 8),
                (Platform::LinuxAarch64, 16),
                (Platform::Osx64, 21),
                (Platform::OsxArm64, 22),
            ],
            &goodwithtech_dockle_dockle_names(),
            "dockle",
        );
    }

    fn goss_org_goss_goss_names() -> Vec<&'static str> {
        vec![
            "dcgoss",
            "dcgoss.sha256",
            "dgoss",
            "dgoss.sha256",
            "goss-darwin-amd64",
            "goss-darwin-amd64.sha256",
            "goss-darwin-arm64",
            "goss-darwin-arm64.sha256",
            "goss-linux-386",
            "goss-linux-386.sha256",
            "goss-linux-amd64",
            "goss-linux-amd64.sha256",
            "goss-linux-arm",
            "goss-linux-arm.sha256",
            "goss-linux-arm64",
            "goss-linux-arm64.sha256",
            "goss-linux-s390x",
            "goss-linux-s390x.sha256",
            "goss-windows-amd64.exe",
            "goss-windows-amd64.exe.sha256",
            "kgoss",
            "kgoss.sha256",
        ]
    }

    #[test]
    fn test_goss_org_goss_goss_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 8),
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 14),
            ],
            &goss_org_goss_goss_names(),
            "goss",
        );
    }

    fn gptscript_ai_gptscript_gptscript_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "gptscript-v0.9.8-linux-amd64.tar.gz",
            "gptscript-v0.9.8-linux-arm64.tar.gz",
            "gptscript-v0.9.8-macOS-universal.tar.gz",
            "gptscript-v0.9.8-windows-amd64.zip",
            "gptscript-v0.9.8-windows-arm64.zip",
        ]
    }

    #[test]
    fn test_gptscript_ai_gptscript_gptscript_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 4),
                (Platform::WinArm64, 5),
            ],
            &gptscript_ai_gptscript_gptscript_names(),
            "gptscript",
        );
    }

    fn graelo_pumas_pumas_names() -> Vec<&'static str> {
        vec![
            "pumas-aarch64-apple-darwin.zip",
        ]
    }

    #[test]
    fn test_graelo_pumas_pumas_names() {
        platform_match_test(
            &[
                (Platform::OsxArm64, 0),
            ],
            &graelo_pumas_pumas_names(),
            "pumas",
        );
    }

    fn grafana_k6_k6_names() -> Vec<&'static str> {
        vec![
            "k6-v1.6.1-checksums.txt",
            "k6-v1.6.1-linux-amd64.deb",
            "k6-v1.6.1-linux-amd64.rpm",
            "k6-v1.6.1-linux-amd64.tar.gz",
            "k6-v1.6.1-linux-arm64.tar.gz",
            "k6-v1.6.1-macos-amd64.zip",
            "k6-v1.6.1-macos-arm64.zip",
            "k6-v1.6.1-spdx.json",
            "k6-v1.6.1-windows-amd64.msi",
            "k6-v1.6.1-windows-amd64.zip",
        ]
    }

    #[test]
    fn test_grafana_k6_k6_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 6),
                (Platform::Win64, 9),
            ],
            &grafana_k6_k6_names(),
            "k6",
        );
    }

    fn grafana_xk6_xk6_names() -> Vec<&'static str> {
        vec![
            "xk6_1.3.5_checksums.txt",
            "xk6_1.3.5_darwin_amd64.tar.gz",
            "xk6_1.3.5_darwin_arm64.tar.gz",
            "xk6_1.3.5_linux_amd64.tar.gz",
            "xk6_1.3.5_linux_arm64.tar.gz",
            "xk6_1.3.5_source.tar.gz",
            "xk6_1.3.5_windows_amd64.zip",
            "xk6_1.3.5_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_grafana_xk6_xk6_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 4),
            ],
            &grafana_xk6_xk6_names(),
            "xk6",
        );
    }

    fn greymd_teip_teip_names() -> Vec<&'static str> {
        vec![
            "teip-2.3.2.aarch64-apple-darwin.tar.gz",
            "teip-2.3.2.aarch64-apple-darwin.tar.gz.sha256",
            "teip-2.3.2.aarch64-unknown-linux-musl.deb",
            "teip-2.3.2.aarch64-unknown-linux-musl.deb.sha256",
            "teip-2.3.2.aarch64-unknown-linux-musl.rpm",
            "teip-2.3.2.aarch64-unknown-linux-musl.rpm.sha256",
            "teip-2.3.2.aarch64-unknown-linux-musl.tar.gz",
            "teip-2.3.2.aarch64-unknown-linux-musl.tar.gz.sha256",
            "teip-2.3.2.arm-unknown-linux-gnueabihf.deb",
            "teip-2.3.2.arm-unknown-linux-gnueabihf.deb.sha256",
            "teip-2.3.2.arm-unknown-linux-gnueabihf.tar.gz",
            "teip-2.3.2.arm-unknown-linux-gnueabihf.tar.gz.sha256",
            "teip-2.3.2.x86_64-apple-darwin.tar.gz",
            "teip-2.3.2.x86_64-apple-darwin.tar.gz.sha256",
            "teip-2.3.2.x86_64-unknown-linux-musl.deb",
            "teip-2.3.2.x86_64-unknown-linux-musl.deb.sha256",
            "teip-2.3.2.x86_64-unknown-linux-musl.rpm",
            "teip-2.3.2.x86_64-unknown-linux-musl.rpm.sha256",
            "teip-2.3.2.x86_64-unknown-linux-musl.tar.gz",
            "teip-2.3.2.x86_64-unknown-linux-musl.tar.gz.sha256",
            "teip_installer-2.3.2-x86_64-pc-windows-msvc.exe",
            "teip_installer-2.3.2-x86_64-pc-windows-msvc.exe.sha256",
        ]
    }

    #[test]
    fn test_greymd_teip_teip_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 18),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 12),
                (Platform::OsxArm64, 0),
            ],
            &greymd_teip_teip_names(),
            "teip",
        );
    }

    fn grpc_ecosystem_grpc_gateway_protoc_gen_grpc_gateway_names() -> Vec<&'static str> {
        vec![
            "grpc-gateway-v2.28.0.tar.gz",
            "grpc-gateway_2.28.0_checksums.txt",
            "multiple.intoto.jsonl",
            "protoc-gen-grpc-gateway-v2.28.0-darwin-arm64",
            "protoc-gen-grpc-gateway-v2.28.0-darwin-x86_64",
            "protoc-gen-grpc-gateway-v2.28.0-linux-arm64",
            "protoc-gen-grpc-gateway-v2.28.0-linux-x86_64",
            "protoc-gen-grpc-gateway-v2.28.0-windows-arm64.exe",
            "protoc-gen-grpc-gateway-v2.28.0-windows-x86_64.exe",
            "protoc-gen-openapiv2-v2.28.0-darwin-arm64",
            "protoc-gen-openapiv2-v2.28.0-darwin-x86_64",
            "protoc-gen-openapiv2-v2.28.0-linux-arm64",
            "protoc-gen-openapiv2-v2.28.0-linux-x86_64",
            "protoc-gen-openapiv2-v2.28.0-windows-arm64.exe",
            "protoc-gen-openapiv2-v2.28.0-windows-x86_64.exe",
        ]
    }

    #[test]
    fn test_grpc_ecosystem_grpc_gateway_protoc_gen_grpc_gateway_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 3),
            ],
            &grpc_ecosystem_grpc_gateway_protoc_gen_grpc_gateway_names(),
            "protoc-gen-grpc-gateway",
        );
    }

    fn grpc_ecosystem_grpc_gateway_protoc_gen_openapiv2_names() -> Vec<&'static str> {
        vec![
            "grpc-gateway-v2.28.0.tar.gz",
            "grpc-gateway_2.28.0_checksums.txt",
            "multiple.intoto.jsonl",
            "protoc-gen-grpc-gateway-v2.28.0-darwin-arm64",
            "protoc-gen-grpc-gateway-v2.28.0-darwin-x86_64",
            "protoc-gen-grpc-gateway-v2.28.0-linux-arm64",
            "protoc-gen-grpc-gateway-v2.28.0-linux-x86_64",
            "protoc-gen-grpc-gateway-v2.28.0-windows-arm64.exe",
            "protoc-gen-grpc-gateway-v2.28.0-windows-x86_64.exe",
            "protoc-gen-openapiv2-v2.28.0-darwin-arm64",
            "protoc-gen-openapiv2-v2.28.0-darwin-x86_64",
            "protoc-gen-openapiv2-v2.28.0-linux-arm64",
            "protoc-gen-openapiv2-v2.28.0-linux-x86_64",
            "protoc-gen-openapiv2-v2.28.0-windows-arm64.exe",
            "protoc-gen-openapiv2-v2.28.0-windows-x86_64.exe",
        ]
    }

    #[test]
    fn test_grpc_ecosystem_grpc_gateway_protoc_gen_openapiv2_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 12),
                (Platform::LinuxAarch64, 11),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 9),
            ],
            &grpc_ecosystem_grpc_gateway_protoc_gen_openapiv2_names(),
            "protoc-gen-openapiv2",
        );
    }

    fn gsamokovarov_jump_jump_names() -> Vec<&'static str> {
        vec![
            "jump-0.67.0-1.x86_64.rpm",
            "jump_0.67.0_amd64.deb",
            "jump_linux_amd64_binary",
            "jump_linux_arm_binary",
            "jump_windows_amd64_binary.exe",
        ]
    }

    #[test]
    fn test_gsamokovarov_jump_jump_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 3),
            ],
            &gsamokovarov_jump_jump_names(),
            "jump",
        );
    }

    fn guumaster_hostctl_hostctl_names() -> Vec<&'static str> {
        vec![
            "hostctl_1.1.4_checksums.txt",
            "hostctl_1.1.4_linux_64-bit.tar.gz",
            "hostctl_1.1.4_linux_amd64.deb",
            "hostctl_1.1.4_linux_arm64.deb",
            "hostctl_1.1.4_linux_arm64.tar.gz",
            "hostctl_1.1.4_macOS_64-bit.tar.gz",
            "hostctl_1.1.4_macOS_arm64.tar.gz",
            "hostctl_1.1.4_windows_64-bit.zip",
            "hostctl_1.1.4_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_guumaster_hostctl_hostctl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 6),
                (Platform::Win64, 7),
                (Platform::WinArm64, 8),
            ],
            &guumaster_hostctl_hostctl_names(),
            "hostctl",
        );
    }

    fn hadolint_hadolint_hadolint_names() -> Vec<&'static str> {
        vec![
            "hadolint-linux-arm64",
            "hadolint-linux-arm64.sha256",
            "hadolint-linux-x86_64",
            "hadolint-linux-x86_64.sha256",
            "hadolint-macos-arm64",
            "hadolint-macos-arm64.sha256",
            "hadolint-macos-x86_64",
            "hadolint-macos-x86_64.sha256",
            "hadolint-windows-x86_64.exe",
            "hadolint-windows-x86_64.exe.sha256",
        ]
    }

    #[test]
    fn test_hadolint_hadolint_hadolint_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 4),
            ],
            &hadolint_hadolint_hadolint_names(),
            "hadolint",
        );
    }

    fn hatoo_oha_oha_names() -> Vec<&'static str> {
        vec![
            "oha-linux-amd64",
            "oha-linux-amd64-pgo",
            "oha-linux-arm64",
            "oha-macos-amd64",
            "oha-macos-arm64",
            "oha-windows-amd64-pgo.exe",
            "oha-windows-amd64.exe",
        ]
    }

    #[test]
    fn test_hatoo_oha_oha_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 4),
            ],
            &hatoo_oha_oha_names(),
            "oha",
        );
    }

    fn helix_editor_helix_helix_names() -> Vec<&'static str> {
        vec![
            "helix-25.07.1-aarch64-linux.tar.xz",
            "helix-25.07.1-aarch64-macos.tar.xz",
            "helix-25.07.1-source.tar.xz",
            "helix-25.07.1-x86_64-linux.tar.xz",
            "helix-25.07.1-x86_64-macos.tar.xz",
            "helix-25.07.1-x86_64-windows.zip",
            "helix-25.07.1-x86_64.AppImage",
            "helix-25.07.1-x86_64.AppImage.zsync",
            "helix_25.7.1-1_amd64.deb",
        ]
    }

    #[test]
    fn test_helix_editor_helix_helix_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 5),
            ],
            &helix_editor_helix_helix_names(),
            "helix",
        );
    }

    fn hellux_jotdown_jotdown_names() -> Vec<&'static str> {
        vec![
            "jotdown-0.9.1-aarch64-apple-darwin.tar.gz",
            "jotdown-0.9.1-i686-pc-windows-msvc.zip",
            "jotdown-0.9.1-i686-unknown-linux-musl.tar.gz",
            "jotdown-0.9.1-x86_64-apple-darwin.tar.gz",
            "jotdown-0.9.1-x86_64-pc-windows-msvc.zip",
            "jotdown-0.9.1-x86_64-unknown-linux-gnu.tar.gz",
            "jotdown_wasm.js",
            "jotdown_wasm_bg.wasm",
        ]
    }

    #[test]
    fn test_hellux_jotdown_jotdown_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 4),
            ],
            &hellux_jotdown_jotdown_names(),
            "jotdown",
        );
    }

    fn hirosassa_ksnotify_ksnotify_names() -> Vec<&'static str> {
        vec![
            "ksnotify-x86_64-darwin.tar.gz",
            "ksnotify-x86_64-linux.tar.gz",
            "ksnotify-x86_64-windows.zip",
        ]
    }

    #[test]
    fn test_hirosassa_ksnotify_ksnotify_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 0),
                (Platform::Win64, 2),
            ],
            &hirosassa_ksnotify_ksnotify_names(),
            "ksnotify",
        );
    }

    fn hougesen_mdsf_mdsf_names() -> Vec<&'static str> {
        vec![
            "dist-manifest.json",
            "mdsf-aarch64-apple-darwin.tar.gz",
            "mdsf-aarch64-apple-darwin.tar.gz.sha256",
            "mdsf-installer.ps1",
            "mdsf-installer.sh",
            "mdsf-npm-package.tar.gz",
            "mdsf-x86_64-apple-darwin.tar.gz",
            "mdsf-x86_64-apple-darwin.tar.gz.sha256",
            "mdsf-x86_64-pc-windows-msvc.msi",
            "mdsf-x86_64-pc-windows-msvc.msi.sha256",
            "mdsf-x86_64-pc-windows-msvc.tar.gz",
            "mdsf-x86_64-pc-windows-msvc.tar.gz.sha256",
            "mdsf-x86_64-unknown-linux-gnu.tar.gz",
            "mdsf-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "mdsf.rb",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_hougesen_mdsf_mdsf_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 12),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 10),
            ],
            &hougesen_mdsf_mdsf_names(),
            "mdsf",
        );
    }

    fn houseabsolute_omegasort_omegasort_names() -> Vec<&'static str> {
        vec![
            "omegasort-Darwin-aarch64.tar.gz",
            "omegasort-Darwin-x86_64.tar.gz",
            "omegasort-FreeBSD-x86_64.tar.gz",
            "omegasort-Linux-aarch64-musl.tar.gz",
            "omegasort-Linux-arm-musl.tar.gz",
            "omegasort-Linux-i686-musl.tar.gz",
            "omegasort-Linux-powerpc-gnu.tar.gz",
            "omegasort-Linux-powerpc64-gnu.tar.gz",
            "omegasort-Linux-powerpc64le.tar.gz",
            "omegasort-Linux-riscv64gc-gnu.tar.gz",
            "omegasort-Linux-s390x-gnu.tar.gz",
            "omegasort-Linux-x86_64-musl.tar.gz",
            "omegasort-NetBSD-x86_64.tar.gz",
            "omegasort-Windows-aarch64.zip",
            "omegasort-Windows-i686.zip",
            "omegasort-Windows-x86_64.zip",
        ]
    }

    #[test]
    fn test_houseabsolute_omegasort_omegasort_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 11),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 15),
                (Platform::WinArm64, 13),
            ],
            &houseabsolute_omegasort_omegasort_names(),
            "omegasort",
        );
    }

    fn houseabsolute_ubi_ubi_names() -> Vec<&'static str> {
        vec![
            "ubi-FreeBSD-x86_64.tar.gz",
            "ubi-FreeBSD-x86_64.tar.gz.sha256",
            "ubi-Linux-gnu-powerpc.tar.gz",
            "ubi-Linux-gnu-powerpc.tar.gz.sha256",
            "ubi-Linux-gnu-powerpc64.tar.gz",
            "ubi-Linux-gnu-powerpc64.tar.gz.sha256",
            "ubi-Linux-gnu-powerpc64le.tar.gz",
            "ubi-Linux-gnu-powerpc64le.tar.gz.sha256",
            "ubi-Linux-gnu-riscv64gc.tar.gz",
            "ubi-Linux-gnu-riscv64gc.tar.gz.sha256",
            "ubi-Linux-gnu-s390x.tar.gz",
            "ubi-Linux-gnu-s390x.tar.gz.sha256",
            "ubi-Linux-musl-arm64.tar.gz",
            "ubi-Linux-musl-arm64.tar.gz.sha256",
            "ubi-Linux-musl-i686.tar.gz",
            "ubi-Linux-musl-i686.tar.gz.sha256",
            "ubi-Linux-musl-x86_64.tar.gz",
            "ubi-Linux-musl-x86_64.tar.gz.sha256",
            "ubi-Linux-musleabi-arm.tar.gz",
            "ubi-Linux-musleabi-arm.tar.gz.sha256",
            "ubi-macOS-arm64.tar.gz",
            "ubi-macOS-arm64.tar.gz.sha256",
            "ubi-macOS-x86_64.tar.gz",
            "ubi-macOS-x86_64.tar.gz.sha256",
            "ubi-NetBSD-x86_64.tar.gz",
            "ubi-NetBSD-x86_64.tar.gz.sha256",
            "ubi-Windows-msvc-arm64.zip",
            "ubi-Windows-msvc-arm64.zip.sha256",
            "ubi-Windows-msvc-i686.zip",
            "ubi-Windows-msvc-i686.zip.sha256",
            "ubi-Windows-msvc-x86_64.zip",
            "ubi-Windows-msvc-x86_64.zip.sha256",
        ]
    }

    #[test]
    fn test_houseabsolute_ubi_ubi_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 16),
                (Platform::LinuxAarch64, 12),
                (Platform::Osx64, 22),
                (Platform::OsxArm64, 20),
                (Platform::Win64, 30),
                (Platform::WinArm64, 26),
            ],
            &houseabsolute_ubi_ubi_names(),
            "ubi",
        );
    }

    fn iffse_pay_respects_pay_respects_names() -> Vec<&'static str> {
        vec![
            "pay-respects-0.7.12-1.aarch64.rpm",
            "pay-respects-0.7.12-1.armv7.rpm",
            "pay-respects-0.7.12-1.i686.rpm",
            "pay-respects-0.7.12-1.x86_64.rpm",
            "pay-respects-0.7.12-aarch64-apple-darwin.tar.zst",
            "pay-respects-0.7.12-aarch64-linux-android.tar.zst",
            "pay-respects-0.7.12-aarch64-pc-windows-msvc.zip",
            "pay-respects-0.7.12-aarch64-unknown-linux-musl.tar.zst",
            "pay-respects-0.7.12-armv7-unknown-linux-musleabihf.tar.zst",
            "pay-respects-0.7.12-i686-unknown-linux-musl.tar.zst",
            "pay-respects-0.7.12-x86_64-apple-darwin.tar.zst",
            "pay-respects-0.7.12-x86_64-pc-windows-msvc.zip",
            "pay-respects-0.7.12-x86_64-unknown-linux-musl.tar.zst",
            "pay-respects_0.7.12-1_amd64.deb",
            "pay-respects_0.7.12-1_arm64.deb",
            "pay-respects_0.7.12-1_armhf.deb",
            "pay-respects_0.7.12-1_i386.deb",
        ]
    }

    #[test]
    fn test_iffse_pay_respects_pay_respects_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 12),
                (Platform::LinuxAarch64, 7),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 11),
                (Platform::WinArm64, 6),
            ],
            &iffse_pay_respects_pay_respects_names(),
            "pay-respects",
        );
    }

    fn igor_petruk_scriptisto_scriptisto_names() -> Vec<&'static str> {
        vec![
            "scriptisto-2.2.0-1.x86_64.rpm",
            "scriptisto-x86_64-apple-darwin.tar.bz2",
            "scriptisto-x86_64-unknown-linux-musl.tar.bz2",
            "scriptisto_2.2.0-1_amd64.deb",
        ]
    }

    #[test]
    fn test_igor_petruk_scriptisto_scriptisto_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 1),
            ],
            &igor_petruk_scriptisto_scriptisto_names(),
            "scriptisto",
        );
    }

    fn imuxin_kubectl_watch_kubectl_watch_names() -> Vec<&'static str> {
        vec![
            "kubectl-watch-aarch64-unknown-linux-gnu.tar.gz",
            "kubectl-watch-x86_64-apple-darwin.tar.gz",
            "kubectl-watch-x86_64-unknown-linux-gnu.tar.gz",
        ]
    }

    #[test]
    fn test_imuxin_kubectl_watch_kubectl_watch_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 1),
            ],
            &imuxin_kubectl_watch_kubectl_watch_names(),
            "kubectl-watch",
        );
    }

    fn ismaelgv_rnr_rnr_names() -> Vec<&'static str> {
        vec![
            "rnr-v0.5.1-aarch64-unknown-linux-gnu.tar.gz",
            "rnr-v0.5.1-armv7-unknown-linux-gnueabihf.tar.gz",
            "rnr-v0.5.1-x86_64-apple-darwin.tar.gz",
            "rnr-v0.5.1-x86_64-pc-windows-gnu.zip",
            "rnr-v0.5.1-x86_64-pc-windows-msvc.zip",
            "rnr-v0.5.1-x86_64-unknown-linux-gnu.tar.gz",
            "rnr-v0.5.1-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_ismaelgv_rnr_rnr_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 2),
                (Platform::Win64, 4),
            ],
            &ismaelgv_rnr_rnr_names(),
            "rnr",
        );
    }

    fn istio_istio_istioctl_names() -> Vec<&'static str> {
        vec![
            "istio-1.29.0-linux-amd64.tar.gz",
            "istio-1.29.0-linux-amd64.tar.gz.sha256",
            "istio-1.29.0-linux-arm64.tar.gz",
            "istio-1.29.0-linux-arm64.tar.gz.sha256",
            "istio-1.29.0-linux-armv7.tar.gz",
            "istio-1.29.0-linux-armv7.tar.gz.sha256",
            "istio-1.29.0-osx-amd64.tar.gz",
            "istio-1.29.0-osx-amd64.tar.gz.sha256",
            "istio-1.29.0-osx-arm64.tar.gz",
            "istio-1.29.0-osx-arm64.tar.gz.sha256",
            "istio-1.29.0-osx.tar.gz",
            "istio-1.29.0-osx.tar.gz.sha256",
            "istio-1.29.0-win-amd64.zip",
            "istio-1.29.0-win-amd64.zip.sha256",
            "istio-1.29.0-win.zip",
            "istio-1.29.0-win.zip.sha256",
            "istio-release.spdx",
            "istio-source.spdx",
            "istioctl-1.29.0-linux-amd64.tar.gz",
            "istioctl-1.29.0-linux-amd64.tar.gz.sha256",
            "istioctl-1.29.0-linux-arm64.tar.gz",
            "istioctl-1.29.0-linux-arm64.tar.gz.sha256",
            "istioctl-1.29.0-linux-armv7.tar.gz",
            "istioctl-1.29.0-linux-armv7.tar.gz.sha256",
            "istioctl-1.29.0-osx-amd64.tar.gz",
            "istioctl-1.29.0-osx-amd64.tar.gz.sha256",
            "istioctl-1.29.0-osx-arm64.tar.gz",
            "istioctl-1.29.0-osx-arm64.tar.gz.sha256",
            "istioctl-1.29.0-osx.tar.gz",
            "istioctl-1.29.0-osx.tar.gz.sha256",
            "istioctl-1.29.0-win-amd64.zip",
            "istioctl-1.29.0-win-amd64.zip.sha256",
            "istioctl-1.29.0-win.zip",
            "istioctl-1.29.0-win.zip.sha256",
        ]
    }

    #[test]
    fn test_istio_istio_istioctl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 18),
                (Platform::LinuxAarch64, 20),
                (Platform::Osx64, 28),
                (Platform::OsxArm64, 26),
                (Platform::Win64, 32),
            ],
            &istio_istio_istioctl_names(),
            "istioctl",
        );
    }

    fn itamae_kitchen_mitamae_mitamae_names() -> Vec<&'static str> {
        vec![
            "mitamae-aarch64-darwin",
            "mitamae-aarch64-darwin.tar.gz",
            "mitamae-aarch64-linux",
            "mitamae-aarch64-linux.tar.gz",
            "mitamae-armhf-linux",
            "mitamae-armhf-linux.tar.gz",
            "mitamae-i386-linux",
            "mitamae-i386-linux.tar.gz",
            "mitamae-x86_64-darwin",
            "mitamae-x86_64-darwin.tar.gz",
            "mitamae-x86_64-linux",
            "mitamae-x86_64-linux.tar.gz",
            "SHA256SUMS",
        ]
    }

    #[test]
    fn test_itamae_kitchen_mitamae_mitamae_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 11),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 1),
            ],
            &itamae_kitchen_mitamae_mitamae_names(),
            "mitamae",
        );
    }

    fn iyear_tdl_tdl_names() -> Vec<&'static str> {
        vec![
            "tdl_checksums.txt",
            "tdl_Linux_32bit.tar.gz",
            "tdl_Linux_64bit.tar.gz",
            "tdl_Linux_arm64.tar.gz",
            "tdl_Linux_armv5.tar.gz",
            "tdl_Linux_armv6.tar.gz",
            "tdl_Linux_armv7.tar.gz",
            "tdl_Linux_loong64.tar.gz",
            "tdl_Linux_riscv64.tar.gz",
            "tdl_MacOS_64bit.tar.gz",
            "tdl_MacOS_arm64.tar.gz",
            "tdl_Windows_32bit.zip",
            "tdl_Windows_64bit.zip",
            "tdl_Windows_arm64.zip",
            "tdl_Windows_armv5.zip",
            "tdl_Windows_armv6.zip",
            "tdl_Windows_armv7.zip",
        ]
    }

    #[test]
    fn test_iyear_tdl_tdl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 10),
                (Platform::Win64, 12),
                (Platform::WinArm64, 13),
            ],
            &iyear_tdl_tdl_names(),
            "tdl",
        );
    }

    fn jdx_hk_hk_names() -> Vec<&'static str> {
        vec![
            "hk-aarch64-apple-darwin.tar.gz",
            "hk-aarch64-pc-windows-msvc.zip",
            "hk-aarch64-unknown-linux-gnu.tar.gz",
            "hk-x86_64-pc-windows-msvc.zip",
            "hk-x86_64-unknown-linux-gnu.tar.gz",
            "hk@1.36.0",
            "hk@1.36.0.sha256",
            "hk@1.36.0.zip",
            "hk@1.36.0.zip.sha256",
        ]
    }

    #[test]
    fn test_jdx_hk_hk_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 2),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 3),
                (Platform::WinArm64, 1),
            ],
            &jdx_hk_hk_names(),
            "hk",
        );
    }

    fn jdx_mise_mise_names() -> Vec<&'static str> {
        vec![
            "install.sh",
            "install.sh.minisig",
            "install.sh.sig",
            "mise-v2026.3.1-linux-arm64",
            "mise-v2026.3.1-linux-arm64-musl",
            "mise-v2026.3.1-linux-arm64-musl.tar.gz",
            "mise-v2026.3.1-linux-arm64-musl.tar.xz",
            "mise-v2026.3.1-linux-arm64-musl.tar.zst",
            "mise-v2026.3.1-linux-arm64.tar.gz",
            "mise-v2026.3.1-linux-arm64.tar.xz",
            "mise-v2026.3.1-linux-arm64.tar.zst",
            "mise-v2026.3.1-linux-armv7",
            "mise-v2026.3.1-linux-armv7-musl",
            "mise-v2026.3.1-linux-armv7-musl.tar.gz",
            "mise-v2026.3.1-linux-armv7-musl.tar.xz",
            "mise-v2026.3.1-linux-armv7-musl.tar.zst",
            "mise-v2026.3.1-linux-armv7.tar.gz",
            "mise-v2026.3.1-linux-armv7.tar.xz",
            "mise-v2026.3.1-linux-armv7.tar.zst",
            "mise-v2026.3.1-linux-x64",
            "mise-v2026.3.1-linux-x64-musl",
            "mise-v2026.3.1-linux-x64-musl.tar.gz",
            "mise-v2026.3.1-linux-x64-musl.tar.xz",
            "mise-v2026.3.1-linux-x64-musl.tar.zst",
            "mise-v2026.3.1-linux-x64.tar.gz",
            "mise-v2026.3.1-linux-x64.tar.xz",
            "mise-v2026.3.1-linux-x64.tar.zst",
            "mise-v2026.3.1-macos-arm64",
            "mise-v2026.3.1-macos-arm64.tar.gz",
            "mise-v2026.3.1-macos-arm64.tar.xz",
            "mise-v2026.3.1-macos-arm64.tar.zst",
            "mise-v2026.3.1-macos-x64",
            "mise-v2026.3.1-macos-x64.tar.gz",
            "mise-v2026.3.1-macos-x64.tar.xz",
            "mise-v2026.3.1-macos-x64.tar.zst",
            "mise-v2026.3.1-windows-arm64.zip",
            "mise-v2026.3.1-windows-x64.zip",
            "SHASUMS256.asc",
            "SHASUMS256.txt",
            "SHASUMS256.txt.minisig",
            "SHASUMS512.asc",
            "SHASUMS512.txt",
            "SHASUMS512.txt.minisig",
            "v2026.3.1.tar.gz.sig",
        ]
    }

    #[test]
    fn test_jdx_mise_mise_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 21),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 32),
                (Platform::OsxArm64, 28),
            ],
            &jdx_mise_mise_names(),
            "mise",
        );
    }

    fn jdx_usage_usage_names() -> Vec<&'static str> {
        vec![
            "usage-aarch64-pc-windows-msvc.zip",
            "usage-aarch64-unknown-linux-gnu.tar.gz",
            "usage-aarch64-unknown-linux-musl.tar.gz",
            "usage-universal-apple-darwin.tar.gz",
            "usage-x86_64-pc-windows-msvc.zip",
            "usage-x86_64-unknown-linux-gnu.tar.gz",
            "usage-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_jdx_usage_usage_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 3),
            ],
            &jdx_usage_usage_names(),
            "usage",
        );
    }

    fn jedisct1_minisign_minisign_names() -> Vec<&'static str> {
        vec![
            "minisign-0.12-linux.tar.gz",
            "minisign-0.12-linux.tar.gz.minisig",
            "minisign-0.12-macos.zip",
            "minisign-0.12-macos.zip.minisig",
            "minisign-0.12-wasm.gz",
            "minisign-0.12-wasm.gz.minisig",
            "minisign-0.12-win64.zip",
            "minisign-0.12-win64.zip.minisig",
            "minisign-0.12.tar.gz",
            "minisign-0.12.tar.gz.minisig",
        ]
    }

    #[test]
    fn test_jedisct1_minisign_minisign_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::OsxArm64, 2),
                (Platform::Win32, 6),
                (Platform::Win64, 6),
                (Platform::WinArm64, 6),
            ],
            &jedisct1_minisign_minisign_names(),
            "minisign",
        );
    }

    fn jedisct1_piknik_piknik_names() -> Vec<&'static str> {
        vec![
            "piknik-dragonflybsd_amd64-0.10.2.tar.gz",
            "piknik-freebsd_amd64-0.10.2.tar.gz",
            "piknik-freebsd_i386-0.10.2.tar.gz",
            "piknik-linux_arm-0.10.2.tar.gz",
            "piknik-linux_i386-0.10.2.tar.gz",
            "piknik-linux_x86_64-0.10.2.tar.gz",
            "piknik-macos-0.10.2.tar.gz",
            "piknik-macos-intel-0.10.2.tar.gz",
            "piknik-netbsd_amd64-0.10.2.tar.gz",
            "piknik-netbsd_i386-0.10.2.tar.gz",
            "piknik-openbsd_amd64-0.10.2.tar.gz",
            "piknik-openbsd_i386-0.10.2.tar.gz",
            "piknik-win32-0.10.2.zip",
            "piknik-win64-0.10.2.zip",
            "piknik-win64-arm64-0.10.2.zip",
        ]
    }

    #[test]
    fn test_jedisct1_piknik_piknik_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 6),
                (Platform::Win32, 13),
                (Platform::Win64, 13),
                (Platform::WinArm64, 13),
            ],
            &jedisct1_piknik_piknik_names(),
            "piknik",
        );
    }

    fn jesseduffield_lazynpm_lazynpm_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "lazynpm_0.1.4_Darwin_32-bit.tar.gz",
            "lazynpm_0.1.4_Darwin_x86_64.tar.gz",
            "lazynpm_0.1.4_freebsd_32-bit.tar.gz",
            "lazynpm_0.1.4_freebsd_arm64.tar.gz",
            "lazynpm_0.1.4_freebsd_armv6.tar.gz",
            "lazynpm_0.1.4_freebsd_x86_64.tar.gz",
            "lazynpm_0.1.4_Linux_32-bit.tar.gz",
            "lazynpm_0.1.4_Linux_arm64.tar.gz",
            "lazynpm_0.1.4_Linux_armv6.tar.gz",
            "lazynpm_0.1.4_Linux_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_jesseduffield_lazynpm_lazynpm_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 8),
                (Platform::Osx64, 2),
            ],
            &jesseduffield_lazynpm_lazynpm_names(),
            "lazynpm",
        );
    }

    fn jez_as_tree_as_tree_names() -> Vec<&'static str> {
        vec![
            "as-tree-0.12.0-linux.zip",
            "as-tree-0.12.0-osx.zip",
        ]
    }

    #[test]
    fn test_jez_as_tree_as_tree_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
            ],
            &jez_as_tree_as_tree_names(),
            "as-tree",
        );
    }

    fn jirutka_tty_copy_tty_copy_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "tty-copy.aarch64-linux",
            "tty-copy.armv7-linux",
            "tty-copy.ppc64le-linux",
            "tty-copy.riscv64-linux",
            "tty-copy.x86_64-darwin",
            "tty-copy.x86_64-linux",
        ]
    }

    #[test]
    fn test_jirutka_tty_copy_tty_copy_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 5),
            ],
            &jirutka_tty_copy_tty_copy_names(),
            "tty-copy",
        );
    }

    fn jkfran_killport_killport_names() -> Vec<&'static str> {
        vec![
            "killport-aarch64-apple-darwin.tar.gz",
            "killport-aarch64-linux-gnu.tar.gz",
            "killport-arm-linux-gnueabihf.tar.gz",
            "killport-armv7-linux-gnueabihf.tar.gz",
            "killport-i686-linux-gnu.tar.gz",
            "killport-powerpc64le-linux-gnu.tar.gz",
            "killport-s390x-linux-gnu.tar.gz",
            "killport-x86_64-apple-darwin.tar.gz",
            "killport-x86_64-linux-gnu.tar.gz",
            "killport-x86_64-pc-windows-gnu.tar.gz",
        ]
    }

    #[test]
    fn test_jkfran_killport_killport_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 9),
            ],
            &jkfran_killport_killport_names(),
            "killport",
        );
    }

    fn johnkerl_miller_miller_names() -> Vec<&'static str> {
        vec![
            "miller-6.17.0-1.src.rpm",
            "miller-6.17.0-aix-ppc64.rpm",
            "miller-6.17.0-aix-ppc64.tar.gz",
            "miller-6.17.0-checksums.txt",
            "miller-6.17.0-darwin-amd64.tar.gz",
            "miller-6.17.0-darwin-arm64.tar.gz",
            "miller-6.17.0-freebsd-386.tar.gz",
            "miller-6.17.0-freebsd-amd64.tar.gz",
            "miller-6.17.0-freebsd-arm64.tar.gz",
            "miller-6.17.0-linux-386.deb",
            "miller-6.17.0-linux-386.rpm",
            "miller-6.17.0-linux-386.tar.gz",
            "miller-6.17.0-linux-amd64.deb",
            "miller-6.17.0-linux-amd64.rpm",
            "miller-6.17.0-linux-amd64.tar.gz",
            "miller-6.17.0-linux-arm64.deb",
            "miller-6.17.0-linux-arm64.rpm",
            "miller-6.17.0-linux-arm64.tar.gz",
            "miller-6.17.0-linux-armv6.deb",
            "miller-6.17.0-linux-armv6.rpm",
            "miller-6.17.0-linux-armv6.tar.gz",
            "miller-6.17.0-linux-armv7.deb",
            "miller-6.17.0-linux-armv7.rpm",
            "miller-6.17.0-linux-armv7.tar.gz",
            "miller-6.17.0-linux-ppc64le.deb",
            "miller-6.17.0-linux-ppc64le.rpm",
            "miller-6.17.0-linux-ppc64le.tar.gz",
            "miller-6.17.0-linux-riscv64.deb",
            "miller-6.17.0-linux-riscv64.rpm",
            "miller-6.17.0-linux-riscv64.tar.gz",
            "miller-6.17.0-linux-s390x.deb",
            "miller-6.17.0-linux-s390x.rpm",
            "miller-6.17.0-linux-s390x.tar.gz",
            "miller-6.17.0-windows-386.zip",
            "miller-6.17.0-windows-amd64.zip",
            "miller-6.17.0.tar.gz",
        ]
    }

    #[test]
    fn test_johnkerl_miller_miller_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 11),
                (Platform::Linux64, 14),
                (Platform::LinuxAarch64, 17),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 5),
            ],
            &johnkerl_miller_miller_names(),
            "miller",
        );
    }

    fn jonaslu_ain_ain_names() -> Vec<&'static str> {
        vec![
            "ain_1.6.0_linux_arm64.tar.gz",
            "ain_1.6.0_linux_i386.tar.gz",
            "ain_1.6.0_linux_x86_64.tar.gz",
            "ain_1.6.0_mac_os_arm64.tar.gz",
            "ain_1.6.0_mac_os_x86_64.tar.gz",
            "ain_1.6.0_windows_arm64.zip",
            "ain_1.6.0_windows_i386.zip",
            "ain_1.6.0_windows_x86_64.zip",
            "checksums.txt",
        ]
    }

    #[test]
    fn test_jonaslu_ain_ain_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 7),
                (Platform::WinArm64, 5),
            ],
            &jonaslu_ain_ain_names(),
            "ain",
        );
    }

    fn jpillora_chisel_chisel_names() -> Vec<&'static str> {
        vec![
            "chisel_1.11.4_checksums.txt",
            "chisel_1.11.4_darwin_amd64.gz",
            "chisel_1.11.4_darwin_arm64.gz",
            "chisel_1.11.4_linux_386.apk",
            "chisel_1.11.4_linux_386.deb",
            "chisel_1.11.4_linux_386.gz",
            "chisel_1.11.4_linux_386.rpm",
            "chisel_1.11.4_linux_amd64.apk",
            "chisel_1.11.4_linux_amd64.deb",
            "chisel_1.11.4_linux_amd64.gz",
            "chisel_1.11.4_linux_amd64.rpm",
            "chisel_1.11.4_linux_arm64.apk",
            "chisel_1.11.4_linux_arm64.deb",
            "chisel_1.11.4_linux_arm64.gz",
            "chisel_1.11.4_linux_arm64.rpm",
            "chisel_1.11.4_linux_armv5.apk",
            "chisel_1.11.4_linux_armv5.deb",
            "chisel_1.11.4_linux_armv5.gz",
            "chisel_1.11.4_linux_armv5.rpm",
            "chisel_1.11.4_linux_armv6.apk",
            "chisel_1.11.4_linux_armv6.deb",
            "chisel_1.11.4_linux_armv6.gz",
            "chisel_1.11.4_linux_armv6.rpm",
            "chisel_1.11.4_linux_armv7.apk",
            "chisel_1.11.4_linux_armv7.deb",
            "chisel_1.11.4_linux_armv7.gz",
            "chisel_1.11.4_linux_armv7.rpm",
            "chisel_1.11.4_linux_mips64le_hardfloat.apk",
            "chisel_1.11.4_linux_mips64le_hardfloat.deb",
            "chisel_1.11.4_linux_mips64le_hardfloat.gz",
            "chisel_1.11.4_linux_mips64le_hardfloat.rpm",
            "chisel_1.11.4_linux_mips64le_softfloat.apk",
            "chisel_1.11.4_linux_mips64le_softfloat.deb",
            "chisel_1.11.4_linux_mips64le_softfloat.gz",
            "chisel_1.11.4_linux_mips64le_softfloat.rpm",
            "chisel_1.11.4_linux_mips64_hardfloat.apk",
            "chisel_1.11.4_linux_mips64_hardfloat.deb",
            "chisel_1.11.4_linux_mips64_hardfloat.gz",
            "chisel_1.11.4_linux_mips64_hardfloat.rpm",
            "chisel_1.11.4_linux_mips64_softfloat.apk",
            "chisel_1.11.4_linux_mips64_softfloat.deb",
            "chisel_1.11.4_linux_mips64_softfloat.gz",
            "chisel_1.11.4_linux_mips64_softfloat.rpm",
            "chisel_1.11.4_linux_mipsle_hardfloat.apk",
            "chisel_1.11.4_linux_mipsle_hardfloat.deb",
            "chisel_1.11.4_linux_mipsle_hardfloat.gz",
            "chisel_1.11.4_linux_mipsle_hardfloat.rpm",
            "chisel_1.11.4_linux_mipsle_softfloat.apk",
            "chisel_1.11.4_linux_mipsle_softfloat.deb",
            "chisel_1.11.4_linux_mipsle_softfloat.gz",
            "chisel_1.11.4_linux_mipsle_softfloat.rpm",
            "chisel_1.11.4_linux_mips_hardfloat.apk",
            "chisel_1.11.4_linux_mips_hardfloat.deb",
            "chisel_1.11.4_linux_mips_hardfloat.gz",
            "chisel_1.11.4_linux_mips_hardfloat.rpm",
            "chisel_1.11.4_linux_mips_softfloat.apk",
            "chisel_1.11.4_linux_mips_softfloat.deb",
            "chisel_1.11.4_linux_mips_softfloat.gz",
            "chisel_1.11.4_linux_mips_softfloat.rpm",
            "chisel_1.11.4_linux_ppc64.apk",
            "chisel_1.11.4_linux_ppc64.deb",
            "chisel_1.11.4_linux_ppc64.gz",
            "chisel_1.11.4_linux_ppc64.rpm",
            "chisel_1.11.4_linux_ppc64le.apk",
            "chisel_1.11.4_linux_ppc64le.deb",
            "chisel_1.11.4_linux_ppc64le.gz",
            "chisel_1.11.4_linux_ppc64le.rpm",
            "chisel_1.11.4_linux_s390x.apk",
            "chisel_1.11.4_linux_s390x.deb",
            "chisel_1.11.4_linux_s390x.gz",
            "chisel_1.11.4_linux_s390x.rpm",
            "chisel_1.11.4_openbsd_386.gz",
            "chisel_1.11.4_openbsd_amd64.gz",
            "chisel_1.11.4_openbsd_arm64.gz",
            "chisel_1.11.4_openbsd_armv5.gz",
            "chisel_1.11.4_openbsd_armv6.gz",
            "chisel_1.11.4_openbsd_armv7.gz",
            "chisel_1.11.4_windows_386.zip",
            "chisel_1.11.4_windows_amd64.zip",
            "chisel_1.11.4_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_jpillora_chisel_chisel_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 5),
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 13),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win32, 77),
                (Platform::Win64, 78),
                (Platform::WinArm64, 79),
            ],
            &jpillora_chisel_chisel_names(),
            "chisel",
        );
    }

    fn jreleaser_jreleaser_jreleaser_standalone_names() -> Vec<&'static str> {
        vec![
            "checksums_rmd160.txt",
            "checksums_rmd160.txt.asc",
            "checksums_sha256.txt",
            "checksums_sha256.txt.asc",
            "jreleaser-1.23.0-sboms.zip",
            "jreleaser-1.23.0-sboms.zip.asc",
            "jreleaser-1.23.0.tar",
            "jreleaser-1.23.0.tar.asc",
            "jreleaser-1.23.0.zip",
            "jreleaser-1.23.0.zip.asc",
            "jreleaser-1.23.0.zip.rmd160",
            "jreleaser-1.23.0.zip.sha256",
            "jreleaser-all-1.23.0.intoto.jsonl",
            "jreleaser-ant-tasks-1.23.0.zip",
            "jreleaser-ant-tasks-1.23.0.zip.asc",
            "jreleaser-installer-1.23.0-1.aarch64.rpm",
            "jreleaser-installer-1.23.0-1.aarch64.rpm.asc",
            "jreleaser-installer-1.23.0-1.x86_64.rpm",
            "jreleaser-installer-1.23.0-1.x86_64.rpm.asc",
            "jreleaser-installer-1.23.0-osx-aarch64.pkg",
            "jreleaser-installer-1.23.0-osx-aarch64.pkg.asc",
            "jreleaser-installer-1.23.0-osx-x86_64.pkg",
            "jreleaser-installer-1.23.0-osx-x86_64.pkg.asc",
            "jreleaser-installer-1.23.0-windows-x86_64.msi",
            "jreleaser-installer-1.23.0-windows-x86_64.msi.asc",
            "jreleaser-installer_1.23.0-1_amd64.deb",
            "jreleaser-installer_1.23.0-1_amd64.deb.asc",
            "jreleaser-installer_1.23.0-1_arm64.deb",
            "jreleaser-installer_1.23.0-1_arm64.deb.asc",
            "jreleaser-native-1.23.0-linux-aarch64.zip",
            "jreleaser-native-1.23.0-linux-aarch64.zip.asc",
            "jreleaser-native-1.23.0-linux-x86_64.zip",
            "jreleaser-native-1.23.0-linux-x86_64.zip.asc",
            "jreleaser-native-1.23.0-osx-aarch64.zip",
            "jreleaser-native-1.23.0-osx-aarch64.zip.asc",
            "jreleaser-native-1.23.0-osx-x86_64.zip",
            "jreleaser-native-1.23.0-osx-x86_64.zip.asc",
            "jreleaser-native-1.23.0-windows-x86_64.zip",
            "jreleaser-native-1.23.0-windows-x86_64.zip.asc",
            "jreleaser-standalone-1.23.0-linux-aarch64.zip",
            "jreleaser-standalone-1.23.0-linux-aarch64.zip.asc",
            "jreleaser-standalone-1.23.0-linux-x86_64.zip",
            "jreleaser-standalone-1.23.0-linux-x86_64.zip.asc",
            "jreleaser-standalone-1.23.0-linux_musl-aarch64.zip",
            "jreleaser-standalone-1.23.0-linux_musl-aarch64.zip.asc",
            "jreleaser-standalone-1.23.0-linux_musl-x86_64.zip",
            "jreleaser-standalone-1.23.0-linux_musl-x86_64.zip.asc",
            "jreleaser-standalone-1.23.0-osx-aarch64.zip",
            "jreleaser-standalone-1.23.0-osx-aarch64.zip.asc",
            "jreleaser-standalone-1.23.0-osx-x86_64.zip",
            "jreleaser-standalone-1.23.0-osx-x86_64.zip.asc",
            "jreleaser-standalone-1.23.0-windows-aarch64.zip",
            "jreleaser-standalone-1.23.0-windows-aarch64.zip.asc",
            "jreleaser-standalone-1.23.0-windows-x86_64.zip",
            "jreleaser-standalone-1.23.0-windows-x86_64.zip.asc",
            "jreleaser-tool-provider-1.23.0.jar",
            "jreleaser-tool-provider-1.23.0.jar.asc",
            "VERSION",
        ]
    }

    #[test]
    fn test_jreleaser_jreleaser_jreleaser_standalone_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 45),
                (Platform::LinuxAarch64, 43),
                (Platform::Osx64, 49),
                (Platform::OsxArm64, 47),
                (Platform::Win64, 53),
                (Platform::WinArm64, 51),
            ],
            &jreleaser_jreleaser_jreleaser_standalone_names(),
            "jreleaser-standalone",
        );
    }

    fn jubako_arx_arx_names() -> Vec<&'static str> {
        vec![
            "arx-0.4.1-linux.tar.gz",
            "arx-0.4.1-linux.tar.gz.sha256",
            "arx-0.4.1-macos.tar.gz",
            "arx-0.4.1-macos.tar.gz.sha256",
            "arx-0.4.1-windows.zip",
            "arx-0.4.1-windows.zip.sha256",
            "libarx-0.4.1-cp310-cp310-macosx_11_0_arm64.whl",
            "libarx-0.4.1-cp310-cp310-manylinux_2_34_x86_64.whl",
            "libarx-0.4.1-cp310-cp310-win_amd64.whl",
            "libarx-0.4.1-cp311-cp311-macosx_11_0_arm64.whl",
            "libarx-0.4.1-cp311-cp311-win_amd64.whl",
            "libarx-0.4.1-cp312-cp312-macosx_11_0_arm64.whl",
            "libarx-0.4.1-cp312-cp312-manylinux_2_34_x86_64.whl",
            "libarx-0.4.1-cp312-cp312-win_amd64.whl",
            "libarx-0.4.1-cp313-cp313-macosx_11_0_arm64.whl",
            "libarx-0.4.1-cp313-cp313-win_amd64.whl",
            "libarx-0.4.1-cp314-cp314-macosx_11_0_arm64.whl",
            "libarx-0.4.1-cp314-cp314-win_amd64.whl",
            "libarx-0.4.1-cp39-cp39-win_amd64.whl",
            "libarx-0.4.1.tar.gz",
            "tar2arx-0.4.1-linux.tar.gz",
            "tar2arx-0.4.1-linux.tar.gz.sha256",
            "tar2arx-0.4.1-macos.tar.gz",
            "tar2arx-0.4.1-macos.tar.gz.sha256",
            "tar2arx-0.4.1-windows.zip",
            "tar2arx-0.4.1-windows.zip.sha256",
            "zip2arx-0.4.1-linux.tar.gz",
            "zip2arx-0.4.1-linux.tar.gz.sha256",
            "zip2arx-0.4.1-macos.tar.gz",
            "zip2arx-0.4.1-macos.tar.gz.sha256",
            "zip2arx-0.4.1-windows.zip",
            "zip2arx-0.4.1-windows.zip.sha256",
        ]
    }

    #[test]
    fn test_jubako_arx_arx_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 2),
                (Platform::Win32, 4),
                (Platform::Win64, 4),
                (Platform::WinArm64, 4),
            ],
            &jubako_arx_arx_names(),
            "arx",
        );
    }

    fn kachick_selfup_selfup_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "selfup_Darwin_arm64.tar.gz",
            "selfup_Darwin_x86_64.tar.gz",
            "selfup_Linux_arm64.tar.gz",
            "selfup_Linux_i386.tar.gz",
            "selfup_Linux_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_kachick_selfup_selfup_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
            ],
            &kachick_selfup_selfup_names(),
            "selfup",
        );
    }

    fn kamadorueda_alejandra_alejandra_names() -> Vec<&'static str> {
        vec![
            "alejandra-aarch64-unknown-linux-musl",
            "alejandra-x86_64-unknown-linux-musl",
        ]
    }

    #[test]
    fn test_kamadorueda_alejandra_alejandra_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 0),
            ],
            &kamadorueda_alejandra_alejandra_names(),
            "alejandra",
        );
    }

    fn kastenhq_external_tools_k10tools_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "k10tools_8.5.3_linux_amd64.tar.gz",
            "k10tools_8.5.3_linux_arm64.tar.gz",
            "k10tools_8.5.3_linux_ppc64le.tar.gz",
            "k10tools_8.5.3_macOS_amd64.tar.gz",
            "k10tools_8.5.3_macOS_arm64.tar.gz",
            "k10tools_8.5.3_windows_amd64.zip",
            "k10tools_8.5.3_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_kastenhq_external_tools_k10tools_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 5),
            ],
            &kastenhq_external_tools_k10tools_names(),
            "k10tools",
        );
    }

    fn kastenhq_kubestr_kubestr_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "kubestr_0.4.49_Linux_amd64.tar.gz",
            "kubestr_0.4.49_Linux_arm64.tar.gz",
            "kubestr_0.4.49_MacOS_amd64.tar.gz",
            "kubestr_0.4.49_MacOS_arm64.tar.gz",
            "kubestr_0.4.49_Windows_amd64.tar.gz",
            "kubestr_0.4.49_Windows_arm64.tar.gz",
        ]
    }

    #[test]
    fn test_kastenhq_kubestr_kubestr_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 5),
                (Platform::WinArm64, 6),
            ],
            &kastenhq_kubestr_kubestr_names(),
            "kubestr",
        );
    }

    fn kattouf_progressline_progressline_names() -> Vec<&'static str> {
        vec![
            "progressline-0.2.4-aarch64-unknown-linux-gnu.zip",
            "progressline-0.2.4-arm64-apple-macosx.zip",
            "progressline-0.2.4-x86_64-apple-macosx.zip",
            "progressline-0.2.4-x86_64-unknown-linux-gnu.zip",
        ]
    }

    #[test]
    fn test_kattouf_progressline_progressline_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
            ],
            &kattouf_progressline_progressline_names(),
            "progressline",
        );
    }

    fn kawaz_authsock_filter_authsock_filter_names() -> Vec<&'static str> {
        vec![
            "authsock-filter-aarch64-apple-darwin.tar.gz",
            "authsock-filter-aarch64-unknown-linux-gnu.tar.gz",
            "authsock-filter-x86_64-apple-darwin.tar.gz",
            "authsock-filter-x86_64-unknown-linux-gnu.tar.gz",
        ]
    }

    #[test]
    fn test_kawaz_authsock_filter_authsock_filter_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 0),
            ],
            &kawaz_authsock_filter_authsock_filter_names(),
            "authsock-filter",
        );
    }

    fn kellyjonbrazil_jc_jc_names() -> Vec<&'static str> {
        vec![
            "jc-1.25.6-1.aarch64.rpm",
            "jc-1.25.6-1.x86_64.rpm",
            "jc-1.25.6-darwin-aarch64.tar.gz",
            "jc-1.25.6-darwin-x86_64.tar.gz",
            "jc-1.25.6-linux-aarch64.tar.gz",
            "jc-1.25.6-linux-x86_64.tar.gz",
            "jc-1.25.6-windows.zip",
            "jc-1.25.6.msi",
            "jc_1.25.6-1_amd64.deb",
            "jc_1.25.6-1_arm64.deb",
        ]
    }

    #[test]
    fn test_kellyjonbrazil_jc_jc_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::Osx64, 3),
            ],
            &kellyjonbrazil_jc_jc_names(),
            "jc",
        );
    }

    fn kettle11_devserver_devserver_names() -> Vec<&'static str> {
        vec![
            "devserver-aarch64-apple-darwin.sha512",
            "devserver-aarch64-apple-darwin.tar.gz",
            "devserver-x86_64-apple-darwin.sha512",
            "devserver-x86_64-apple-darwin.tar.gz",
            "devserver-x86_64-pc-windows-msvc.sha512",
            "devserver-x86_64-pc-windows-msvc.zip",
            "devserver-x86_64-unknown-linux-gnu.sha512",
            "devserver-x86_64-unknown-linux-gnu.tar.gz",
        ]
    }

    #[test]
    fn test_kettle11_devserver_devserver_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 5),
            ],
            &kettle11_devserver_devserver_names(),
            "devserver",
        );
    }

    fn kkinnear_zprint_zprint_names() -> Vec<&'static str> {
        vec![
            "appcds",
            "zprint-filter-1.3.0",
            "zprintl-1.3.0",
            "zprintm-1.3.0",
            "zprintma-1.3.0",
        ]
    }

    #[test]
    fn test_kkinnear_zprint_zprint_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 4),
            ],
            &kkinnear_zprint_zprint_names(),
            "zprint",
        );
    }

    fn kluctl_kluctl_kluctl_names() -> Vec<&'static str> {
        vec![
            "kluctl_v2.27.0_checksums.txt",
            "kluctl_v2.27.0_darwin_amd64.tar.gz",
            "kluctl_v2.27.0_darwin_arm64.tar.gz",
            "kluctl_v2.27.0_linux_amd64.tar.gz",
            "kluctl_v2.27.0_linux_arm64.tar.gz",
            "kluctl_v2.27.0_sbom.spdx.json",
            "kluctl_v2.27.0_source_code.tar.gz",
            "kluctl_v2.27.0_windows_amd64.zip",
        ]
    }

    #[test]
    fn test_kluctl_kluctl_kluctl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 7),
            ],
            &kluctl_kluctl_kluctl_names(),
            "kluctl",
        );
    }

    fn ko_build_ko_ko_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "ko_0.18.1_Darwin_arm64.tar.gz",
            "ko_0.18.1_Darwin_x86_64.tar.gz",
            "ko_0.18.1_Linux_arm64.tar.gz",
            "ko_0.18.1_Linux_i386.tar.gz",
            "ko_0.18.1_Linux_mips64le.tar.gz",
            "ko_0.18.1_Linux_ppc64le.tar.gz",
            "ko_0.18.1_Linux_riscv64.tar.gz",
            "ko_0.18.1_Linux_s390x.tar.gz",
            "ko_0.18.1_Linux_x86_64.tar.gz",
            "ko_0.18.1_Windows_arm64.tar.gz",
            "ko_0.18.1_Windows_i386.tar.gz",
            "ko_0.18.1_Windows_x86_64.tar.gz",
            "ko_Darwin_arm64.tar.gz",
            "ko_Darwin_x86_64.tar.gz",
            "ko_Linux_arm64.tar.gz",
            "ko_Linux_i386.tar.gz",
            "ko_Linux_mips64le.tar.gz",
            "ko_Linux_ppc64le.tar.gz",
            "ko_Linux_riscv64.tar.gz",
            "ko_Linux_s390x.tar.gz",
            "ko_Linux_x86_64.tar.gz",
            "ko_Windows_arm64.tar.gz",
            "ko_Windows_i386.tar.gz",
            "ko_Windows_x86_64.tar.gz",
            "multiple.intoto.jsonl",
        ]
    }

    #[test]
    fn test_ko_build_ko_ko_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 4),
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win32, 11),
                (Platform::Win64, 12),
                (Platform::WinArm64, 10),
            ],
            &ko_build_ko_ko_names(),
            "ko",
        );
    }

    fn koalaman_shellcheck_shellcheck_names() -> Vec<&'static str> {
        vec![
            "shellcheck-v0.11.0.darwin.aarch64.tar.gz",
            "shellcheck-v0.11.0.darwin.aarch64.tar.xz",
            "shellcheck-v0.11.0.darwin.x86_64.tar.gz",
            "shellcheck-v0.11.0.darwin.x86_64.tar.xz",
            "shellcheck-v0.11.0.linux.aarch64.tar.gz",
            "shellcheck-v0.11.0.linux.aarch64.tar.xz",
            "shellcheck-v0.11.0.linux.armv6hf.tar.gz",
            "shellcheck-v0.11.0.linux.armv6hf.tar.xz",
            "shellcheck-v0.11.0.linux.riscv64.tar.gz",
            "shellcheck-v0.11.0.linux.riscv64.tar.xz",
            "shellcheck-v0.11.0.linux.x86_64.tar.gz",
            "shellcheck-v0.11.0.linux.x86_64.tar.xz",
            "shellcheck-v0.11.0.zip",
        ]
    }

    #[test]
    fn test_koalaman_shellcheck_shellcheck_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 11),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 1),
                (Platform::Win32, 12),
                (Platform::Win64, 12),
                (Platform::WinArm64, 12),
            ],
            &koalaman_shellcheck_shellcheck_names(),
            "shellcheck",
        );
    }

    fn kopia_kopia_kopia_names() -> Vec<&'static str> {
        vec![
            "builder-debug.yml",
            "CHANGELOG.md",
            "change_log.md",
            "checksums.txt",
            "checksums.txt.sig",
            "kopia-0.22.3-freebsd-experimental-arm.tar.gz",
            "kopia-0.22.3-freebsd-experimental-arm64.tar.gz",
            "kopia-0.22.3-freebsd-experimental-x64.tar.gz",
            "kopia-0.22.3-linux-arm.tar.gz",
            "kopia-0.22.3-linux-arm64.tar.gz",
            "kopia-0.22.3-linux-x64.tar.gz",
            "kopia-0.22.3-macOS-arm64.tar.gz",
            "kopia-0.22.3-macOS-universal.tar.gz",
            "kopia-0.22.3-macOS-x64.tar.gz",
            "kopia-0.22.3-openbsd-experimental-arm.tar.gz",
            "kopia-0.22.3-openbsd-experimental-arm64.tar.gz",
            "kopia-0.22.3-openbsd-experimental-x64.tar.gz",
            "kopia-0.22.3-windows-x64.zip",
            "kopia-0.22.3.aarch64.rpm",
            "kopia-0.22.3.armhfp.rpm",
            "kopia-0.22.3.x86_64.rpm",
            "kopia-ui-0.22.3.aarch64.rpm",
            "kopia-ui-0.22.3.armv7l.rpm",
            "kopia-ui-0.22.3.x86_64.rpm",
            "kopia-ui_0.22.3_amd64.deb",
            "kopia-ui_0.22.3_arm64.deb",
            "kopia-ui_0.22.3_armv7l.deb",
            "KopiaUI-0.22.3-arm64-mac.zip",
            "KopiaUI-0.22.3-arm64.AppImage",
            "KopiaUI-0.22.3-arm64.dmg",
            "KopiaUI-0.22.3-armv7l.AppImage",
            "KopiaUI-0.22.3-mac.zip",
            "KopiaUI-0.22.3-win.zip",
            "KopiaUI-0.22.3.AppImage",
            "KopiaUI-0.22.3.dmg",
            "KopiaUI-Setup-0.22.3.exe",
            "kopia_0.22.3_linux_amd64.deb",
            "kopia_0.22.3_linux_arm64.deb",
            "kopia_0.22.3_linux_armhf.deb",
            "latest-linux-arm.yml",
            "latest-linux-arm64.yml",
            "latest-linux.yml",
            "latest-mac.yml",
            "latest.yml",
        ]
    }

    #[test]
    fn test_kopia_kopia_kopia_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 9),
                (Platform::Osx64, 13),
                (Platform::OsxArm64, 11),
                (Platform::Win64, 17),
            ],
            &kopia_kopia_kopia_names(),
            "kopia",
        );
    }

    fn kubecfg_kubecfg_kubecfg_names() -> Vec<&'static str> {
        vec![
            "kubecfg_Linux_ARM64",
            "kubecfg_Linux_X64",
            "kubecfg_macOS_ARM64",
        ]
    }

    #[test]
    fn test_kubecfg_kubecfg_kubecfg_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::OsxArm64, 2),
            ],
            &kubecfg_kubecfg_kubecfg_names(),
            "kubecfg",
        );
    }

    fn kubernetes_sigs_zeitgeist_zeitgeist_names() -> Vec<&'static str> {
        vec![
            "buoy-amd64-darwin",
            "buoy-amd64-darwin.pem",
            "buoy-amd64-darwin.sig",
            "buoy-amd64-linux",
            "buoy-amd64-linux.pem",
            "buoy-amd64-linux.sig",
            "buoy-amd64-windows.exe",
            "buoy-amd64-windows.exe.pem",
            "buoy-amd64-windows.exe.sig",
            "buoy-arm-linux",
            "buoy-arm-linux.pem",
            "buoy-arm-linux.sig",
            "buoy-arm64-darwin",
            "buoy-arm64-darwin.pem",
            "buoy-arm64-darwin.sig",
            "buoy-arm64-linux",
            "buoy-arm64-linux.pem",
            "buoy-arm64-linux.sig",
            "buoy-bom.json.spdx",
            "buoy-bom.json.spdx.pem",
            "buoy-bom.json.spdx.sig",
            "buoy-ppc64le-linux",
            "buoy-ppc64le-linux.pem",
            "buoy-ppc64le-linux.sig",
            "buoy-s390x-linux",
            "buoy-s390x-linux.pem",
            "buoy-s390x-linux.sig",
            "checksums.txt",
            "checksums.txt.pem",
            "checksums.txt.sig",
            "zeitgeist-amd64-darwin",
            "zeitgeist-amd64-darwin.pem",
            "zeitgeist-amd64-darwin.sig",
            "zeitgeist-amd64-linux",
            "zeitgeist-amd64-linux.pem",
            "zeitgeist-amd64-linux.sig",
            "zeitgeist-amd64-windows.exe",
            "zeitgeist-amd64-windows.exe.pem",
            "zeitgeist-amd64-windows.exe.sig",
            "zeitgeist-arm-linux",
            "zeitgeist-arm-linux.pem",
            "zeitgeist-arm-linux.sig",
            "zeitgeist-arm64-darwin",
            "zeitgeist-arm64-darwin.pem",
            "zeitgeist-arm64-darwin.sig",
            "zeitgeist-arm64-linux",
            "zeitgeist-arm64-linux.pem",
            "zeitgeist-arm64-linux.sig",
            "zeitgeist-bom.json.spdx",
            "zeitgeist-bom.json.spdx.pem",
            "zeitgeist-bom.json.spdx.sig",
            "zeitgeist-ppc64le-linux",
            "zeitgeist-ppc64le-linux.pem",
            "zeitgeist-ppc64le-linux.sig",
            "zeitgeist-remote-amd64-darwin",
            "zeitgeist-remote-amd64-darwin.pem",
            "zeitgeist-remote-amd64-darwin.sig",
            "zeitgeist-remote-amd64-linux",
            "zeitgeist-remote-amd64-linux.pem",
            "zeitgeist-remote-amd64-linux.sig",
            "zeitgeist-remote-amd64-windows.exe",
            "zeitgeist-remote-amd64-windows.exe.pem",
            "zeitgeist-remote-amd64-windows.exe.sig",
            "zeitgeist-remote-arm-linux",
            "zeitgeist-remote-arm-linux.pem",
            "zeitgeist-remote-arm-linux.sig",
            "zeitgeist-remote-arm64-darwin",
            "zeitgeist-remote-arm64-darwin.pem",
            "zeitgeist-remote-arm64-darwin.sig",
            "zeitgeist-remote-arm64-linux",
            "zeitgeist-remote-arm64-linux.pem",
            "zeitgeist-remote-arm64-linux.sig",
            "zeitgeist-remote-bom.json.spdx",
            "zeitgeist-remote-bom.json.spdx.pem",
            "zeitgeist-remote-bom.json.spdx.sig",
            "zeitgeist-remote-ppc64le-linux",
            "zeitgeist-remote-ppc64le-linux.pem",
            "zeitgeist-remote-ppc64le-linux.sig",
            "zeitgeist-remote-s390x-linux",
            "zeitgeist-remote-s390x-linux.pem",
            "zeitgeist-remote-s390x-linux.sig",
            "zeitgeist-s390x-linux",
            "zeitgeist-s390x-linux.pem",
            "zeitgeist-s390x-linux.sig",
            "zeitgeist.intoto.json",
        ]
    }

    #[test]
    fn test_kubernetes_sigs_zeitgeist_zeitgeist_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 33),
                (Platform::LinuxAarch64, 45),
                (Platform::Osx64, 30),
                (Platform::OsxArm64, 42),
            ],
            &kubernetes_sigs_zeitgeist_zeitgeist_names(),
            "zeitgeist",
        );
    }

    fn kubescape_kubescape_kubescape_names() -> Vec<&'static str> {
        vec![
            "checksums.sha256",
            "downloader_4.0.2_linux_amd64.sbom.json",
            "downloader_4.0.2_linux_arm64.sbom.json",
            "ksserver_4.0.2_linux_amd64.sbom.json",
            "ksserver_4.0.2_linux_arm64.sbom.json",
            "kubescape.exe_4.0.2_windows_amd64.sbom.json",
            "kubescape.exe_4.0.2_windows_arm64.sbom.json",
            "kubescape_4.0.2_darwin_amd64",
            "kubescape_4.0.2_darwin_amd64.sbom.json",
            "kubescape_4.0.2_darwin_amd64.tar.gz",
            "kubescape_4.0.2_darwin_arm64",
            "kubescape_4.0.2_darwin_arm64.sbom.json",
            "kubescape_4.0.2_darwin_arm64.tar.gz",
            "kubescape_4.0.2_linux_amd64",
            "kubescape_4.0.2_linux_amd64.apk",
            "kubescape_4.0.2_linux_amd64.deb",
            "kubescape_4.0.2_linux_amd64.rpm",
            "kubescape_4.0.2_linux_amd64.sbom.json",
            "kubescape_4.0.2_linux_amd64.tar.gz",
            "kubescape_4.0.2_linux_arm64",
            "kubescape_4.0.2_linux_arm64.apk",
            "kubescape_4.0.2_linux_arm64.deb",
            "kubescape_4.0.2_linux_arm64.rpm",
            "kubescape_4.0.2_linux_arm64.sbom.json",
            "kubescape_4.0.2_linux_arm64.tar.gz",
            "kubescape_4.0.2_windows_amd64.exe",
            "kubescape_4.0.2_windows_amd64.tar.gz",
            "kubescape_4.0.2_windows_arm64.exe",
            "kubescape_4.0.2_windows_arm64.tar.gz",
        ]
    }

    #[test]
    fn test_kubescape_kubescape_kubescape_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 18),
                (Platform::LinuxAarch64, 24),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 12),
                (Platform::Win64, 26),
                (Platform::WinArm64, 28),
            ],
            &kubescape_kubescape_kubescape_names(),
            "kubescape",
        );
    }

    fn kubevious_cli_kubevious_names() -> Vec<&'static str> {
        vec![
            "kubevious-alpine-arm64",
            "kubevious-alpine-x64",
            "kubevious-linux-arm64",
            "kubevious-linux-x64",
            "kubevious-linuxstatic-arm64",
            "kubevious-linuxstatic-x64",
            "kubevious-macos-arm64",
            "kubevious-macos-x64",
            "kubevious-win-arm64.exe",
            "kubevious-win-x64.exe",
        ]
    }

    #[test]
    fn test_kubevious_cli_kubevious_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 6),
            ],
            &kubevious_cli_kubevious_names(),
            "kubevious",
        );
    }

    fn kubewarden_kwctl_kwctl_names() -> Vec<&'static str> {
        vec![
            "kubewarden-load-policies.sh",
            "kubewarden-save-policies.sh",
            "kwctl-darwin-aarch64-sbom.spdx",
            "kwctl-darwin-aarch64-sbom.spdx.bundle.sigstore",
            "kwctl-darwin-aarch64.zip",
            "kwctl-darwin-x86_64-sbom.spdx",
            "kwctl-darwin-x86_64-sbom.spdx.bundle.sigstore",
            "kwctl-darwin-x86_64.zip",
            "kwctl-linux-aarch64-sbom.spdx",
            "kwctl-linux-aarch64-sbom.spdx.bundle.sigstore",
            "kwctl-linux-aarch64.zip",
            "kwctl-linux-x86_64-sbom.spdx",
            "kwctl-linux-x86_64-sbom.spdx.bundle.sigstore",
            "kwctl-linux-x86_64.zip",
            "kwctl-windows-x86_64-sbom.spdx",
            "kwctl-windows-x86_64-sbom.spdx.bundle.sigstore",
            "kwctl-windows-x86_64.exe.zip",
        ]
    }

    #[test]
    fn test_kubewarden_kwctl_kwctl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 13),
                (Platform::LinuxAarch64, 10),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 16),
            ],
            &kubewarden_kwctl_kwctl_names(),
            "kwctl",
        );
    }

    fn kudobuilder_kuttl_kuttl_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "kubectl-kuttl_0.25.0_darwin_arm64",
            "kubectl-kuttl_0.25.0_darwin_x86_64",
            "kubectl-kuttl_0.25.0_linux_arm64",
            "kubectl-kuttl_0.25.0_linux_armv6",
            "kubectl-kuttl_0.25.0_linux_i386",
            "kubectl-kuttl_0.25.0_linux_ppc64le",
            "kubectl-kuttl_0.25.0_linux_s390x",
            "kubectl-kuttl_0.25.0_linux_x86_64",
            "kuttl_0.25.0_darwin_arm64.tar.gz",
            "kuttl_0.25.0_darwin_x86_64.tar.gz",
            "kuttl_0.25.0_linux_arm64.tar.gz",
            "kuttl_0.25.0_linux_armv6.tar.gz",
            "kuttl_0.25.0_linux_i386.tar.gz",
            "kuttl_0.25.0_linux_ppc64le.tar.gz",
            "kuttl_0.25.0_linux_s390x.tar.gz",
            "kuttl_0.25.0_linux_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_kudobuilder_kuttl_kuttl_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 13),
                (Platform::Linux64, 16),
                (Platform::LinuxAarch64, 11),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 9),
            ],
            &kudobuilder_kuttl_kuttl_names(),
            "kuttl",
        );
    }

    fn kurehajime_kuzusi_kuzusi_names() -> Vec<&'static str> {
        vec![
            "linux_386.zip",
            "linux_amd64.zip",
            "linux_arm.zip",
            "macos_amd64.zip",
            "macos_arm64.zip",
            "windows_386.zip",
            "windows_amd64.zip",
        ]
    }

    #[test]
    fn test_kurehajime_kuzusi_kuzusi_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 4),
                (Platform::Win32, 5),
                (Platform::Win64, 6),
            ],
            &kurehajime_kuzusi_kuzusi_names(),
            "kuzusi",
        );
    }

    fn kurehajime_pong_command_zip_names() -> Vec<&'static str> {
        vec![
            "freebsd_386.zip",
            "freebsd_amd64.zip",
            "freebsd_arm.zip",
            "linux_386.zip",
            "linux_amd64.zip",
            "linux_arm.zip",
            "linux_mips.zip",
            "linux_mips64.zip",
            "linux_mips64le.zip",
            "linux_mipsle.zip",
            "linux_s390x.zip",
            "macos_amd64.zip",
            "macos_arm64.zip",
            "netbsd_386.zip",
            "netbsd_amd64.zip",
            "netbsd_arm.zip",
            "openbsd_386.zip",
            "openbsd_amd64.zip",
            "windows_386.zip",
            "windows_amd64.zip",
        ]
    }

    #[test]
    fn test_kurehajime_pong_command_zip_names() {
        platform_match_test(
            &[
                (Platform::OsxArm64, 12),
            ],
            &kurehajime_pong_command_zip_names(),
            "zip",
        );
    }

    fn legal90_awscurl_awscurl_names() -> Vec<&'static str> {
        vec![
            "awscurl_0.3.0_darwin_amd64.zip",
            "awscurl_0.3.0_darwin_arm64.zip",
            "awscurl_0.3.0_linux_amd64.zip",
            "awscurl_0.3.0_linux_arm64.zip",
            "awscurl_0.3.0_windows_amd64.zip",
            "awscurl_0.3.0_windows_arm64.zip",
            "checksums.txt",
        ]
    }

    #[test]
    fn test_legal90_awscurl_awscurl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 4),
                (Platform::WinArm64, 5),
            ],
            &legal90_awscurl_awscurl_names(),
            "awscurl",
        );
    }

    fn leoafarias_fvm_fvm_names() -> Vec<&'static str> {
        vec![
            "fvm-4.0.5-linux-arm-musl.tar.gz",
            "fvm-4.0.5-linux-arm.tar.gz",
            "fvm-4.0.5-linux-arm64-musl.tar.gz",
            "fvm-4.0.5-linux-arm64.tar.gz",
            "fvm-4.0.5-linux-riscv64-musl.tar.gz",
            "fvm-4.0.5-linux-riscv64.tar.gz",
            "fvm-4.0.5-linux-x64-musl.tar.gz",
            "fvm-4.0.5-linux-x64.tar.gz",
            "fvm-4.0.5-macos-arm64.tar.gz",
            "fvm-4.0.5-macos-x64.tar.gz",
            "fvm-4.0.5-windows-arm64.zip",
            "fvm-4.0.5-windows-x64.zip",
        ]
    }

    #[test]
    fn test_leoafarias_fvm_fvm_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 8),
                (Platform::Win64, 11),
                (Platform::WinArm64, 10),
            ],
            &leoafarias_fvm_fvm_names(),
            "fvm",
        );
    }

    fn lima_vm_lima_lima_names() -> Vec<&'static str> {
        vec![
            "lima-2.0.3-Darwin-arm64.tar.gz",
            "lima-2.0.3-Darwin-x86_64.tar.gz",
            "lima-2.0.3-go-mod-vendor.tar.gz",
            "lima-2.0.3-Linux-aarch64.tar.gz",
            "lima-2.0.3-Linux-x86_64.tar.gz",
            "lima-additional-guestagents-2.0.3-Darwin-arm64.tar.gz",
            "lima-additional-guestagents-2.0.3-Darwin-x86_64.tar.gz",
            "lima-additional-guestagents-2.0.3-Linux-aarch64.tar.gz",
            "lima-additional-guestagents-2.0.3-Linux-x86_64.tar.gz",
            "SHA256SUMS",
            "SHA256SUMS.asc",
        ]
    }

    #[test]
    fn test_lima_vm_lima_lima_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
            ],
            &lima_vm_lima_lima_names(),
            "lima",
        );
    }

    fn linebender_resvg_resvg_names() -> Vec<&'static str> {
        vec![
            "resvg-0.47.0.tar.xz",
            "resvg-explorer-extension.exe",
            "resvg-linux-x86_64.tar.gz",
            "resvg-macos-aarch64.zip",
            "resvg-macos-x86_64.zip",
            "resvg-win64.zip",
            "usvg-linux-x86_64.tar.gz",
            "usvg-macos-aarch64.zip",
            "usvg-macos-x86_64.zip",
            "usvg-win64.zip",
        ]
    }

    #[test]
    fn test_linebender_resvg_resvg_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 4),
                (Platform::Win32, 5),
                (Platform::Win64, 5),
                (Platform::WinArm64, 5),
            ],
            &linebender_resvg_resvg_names(),
            "resvg",
        );
    }

    fn llogick_zigscient_zigscient_names() -> Vec<&'static str> {
        vec![
            "zigscient-aarch64-linux.zip",
            "zigscient-aarch64-macos.zip",
            "zigscient-aarch64-windows.zip",
            "zigscient-x86_64-linux.zip",
            "zigscient-x86_64-macos.zip",
            "zigscient-x86_64-windows.zip",
        ]
    }

    #[test]
    fn test_llogick_zigscient_zigscient_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 5),
                (Platform::WinArm64, 2),
            ],
            &llogick_zigscient_zigscient_names(),
            "zigscient",
        );
    }

    fn luau_lang_luau_luau_names() -> Vec<&'static str> {
        vec![
            "luau-macos.zip",
            "luau-ubuntu.zip",
            "luau-windows.zip",
            "Luau.Web.js",
        ]
    }

    #[test]
    fn test_luau_lang_luau_luau_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 1),
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win32, 2),
                (Platform::Win64, 2),
                (Platform::WinArm64, 2),
            ],
            &luau_lang_luau_luau_names(),
            "luau",
        );
    }

    fn lxc_incus_bin_names() -> Vec<&'static str> {
        vec![
            "bin.linux.incus-agent.aarch64",
            "bin.linux.incus-agent.x86_64",
            "bin.linux.incus-migrate.aarch64",
            "bin.linux.incus-migrate.x86_64",
            "bin.linux.incus.aarch64",
            "bin.linux.incus.x86_64",
            "bin.linux.lxd-to-incus.aarch64",
            "bin.linux.lxd-to-incus.x86_64",
            "bin.macos.incus-agent.aarch64",
            "bin.macos.incus-agent.x86_64",
            "bin.macos.incus.aarch64",
            "bin.macos.incus.x86_64",
            "bin.windows.incus-agent.aarch64.exe",
            "bin.windows.incus-agent.x86_64.exe",
            "bin.windows.incus.aarch64.exe",
            "bin.windows.incus.x86_64.exe",
            "incus-6.22.tar.xz",
            "incus-6.22.tar.xz.asc",
        ]
    }

    #[test]
    fn test_lxc_incus_bin_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 11),
                (Platform::OsxArm64, 10),
            ],
            &lxc_incus_bin_names(),
            "bin",
        );
    }

    fn lycheeverse_lychee_lychee_names() -> Vec<&'static str> {
        vec![
            "lychee-aarch64-unknown-linux-gnu.tar.gz",
            "lychee-aarch64-unknown-linux-musl.tar.gz",
            "lychee-arm-unknown-linux-musleabi.tar.gz",
            "lychee-arm-unknown-linux-musleabihf.tar.gz",
            "lychee-arm64-macos.dmg",
            "lychee-arm64-macos.tar.gz",
            "lychee-armv7-unknown-linux-gnueabihf.tar.gz",
            "lychee-i686-unknown-linux-gnu.tar.gz",
            "lychee-x86_64-unknown-linux-gnu.tar.gz",
            "lychee-x86_64-unknown-linux-musl.tar.gz",
            "lychee-x86_64-windows.exe",
        ]
    }

    #[test]
    fn test_lycheeverse_lychee_lychee_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 0),
                (Platform::OsxArm64, 4),
            ],
            &lycheeverse_lychee_lychee_names(),
            "lychee",
        );
    }

    fn madelynnblue_sqlfmt_sqlfmt_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "sqlfmt_Darwin_x86_64.tar.gz",
            "sqlfmt_Linux_x86_64.tar.gz",
            "sqlfmt_Windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_madelynnblue_sqlfmt_sqlfmt_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 1),
                (Platform::Win64, 3),
            ],
            &madelynnblue_sqlfmt_sqlfmt_names(),
            "sqlfmt",
        );
    }

    fn magefile_mage_mage_names() -> Vec<&'static str> {
        vec![
            "mage_1.15.0_checksums.txt",
            "mage_1.15.0_DragonFlyBSD-64bit.tar.gz",
            "mage_1.15.0_FreeBSD-64bit.tar.gz",
            "mage_1.15.0_FreeBSD-ARM.tar.gz",
            "mage_1.15.0_FreeBSD-ARM64.tar.gz",
            "mage_1.15.0_Linux-64bit.tar.gz",
            "mage_1.15.0_Linux-ARM.tar.gz",
            "mage_1.15.0_Linux-ARM64.tar.gz",
            "mage_1.15.0_macOS-64bit.tar.gz",
            "mage_1.15.0_NetBSD-64bit.tar.gz",
            "mage_1.15.0_NetBSD-ARM.tar.gz",
            "mage_1.15.0_OpenBSD-64bit.tar.gz",
            "mage_1.15.0_OpenBSD-ARM64.tar.gz",
            "mage_1.15.0_Windows-64bit.zip",
            "mage_1.15.0_Windows-ARM.zip",
        ]
    }

    #[test]
    fn test_magefile_mage_mage_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 7),
                (Platform::Osx64, 8),
                (Platform::Win64, 13),
            ],
            &magefile_mage_mage_names(),
            "mage",
        );
    }

    fn makew0rld_didder_didder_names() -> Vec<&'static str> {
        vec![
            "didder_1.3.0_checksums.txt",
            "didder_1.3.0_freebsd_32-bit",
            "didder_1.3.0_freebsd_64-bit",
            "didder_1.3.0_linux_32-bit",
            "didder_1.3.0_linux_64-bit",
            "didder_1.3.0_linux_arm64",
            "didder_1.3.0_linux_armv6",
            "didder_1.3.0_linux_armv7",
            "didder_1.3.0_macOS_64-bit",
            "didder_1.3.0_macOS_arm64",
            "didder_1.3.0_netbsd_32-bit",
            "didder_1.3.0_netbsd_64-bit",
            "didder_1.3.0_openbsd_32-bit",
            "didder_1.3.0_openbsd_64-bit",
            "didder_1.3.0_windows_32-bit.exe",
            "didder_1.3.0_windows_64-bit.exe",
            "didder_1.3.0_windows_arm64.exe",
            "didder_1.3.0_windows_armv6.exe",
            "didder_1.3.0_windows_armv7.exe",
        ]
    }

    #[test]
    fn test_makew0rld_didder_didder_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 8),
                (Platform::OsxArm64, 9),
            ],
            &makew0rld_didder_didder_names(),
            "didder",
        );
    }

    fn making_rsc_rsc_names() -> Vec<&'static str> {
        vec![
            "rsc-0.9.1.jar",
            "rsc-x86_64-apple-darwin",
            "rsc-x86_64-pc-linux",
            "rsc-x86_64-pc-win32.exe",
        ]
    }

    #[test]
    fn test_making_rsc_rsc_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 1),
            ],
            &making_rsc_rsc_names(),
            "rsc",
        );
    }

    fn marp_team_marp_cli_marp_cli_names() -> Vec<&'static str> {
        vec![
            "marp-cli-v4.2.3-linux.tar.gz",
            "marp-cli-v4.2.3-mac.tar.gz",
            "marp-cli-v4.2.3-win.zip",
        ]
    }

    #[test]
    fn test_marp_team_marp_cli_marp_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 2),
            ],
            &marp_team_marp_cli_marp_cli_names(),
            "marp-cli",
        );
    }

    fn mas_cli_mas_mas_names() -> Vec<&'static str> {
        vec![
            "mas-5.2.0-arm64.pkg",
            "mas-5.2.0-x86_64.pkg",
        ]
    }

    #[test]
    fn test_mas_cli_mas_mas_names() {
        platform_match_test(
            &[
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
            ],
            &mas_cli_mas_mas_names(),
            "mas",
        );
    }

    fn matryer_moq_moq_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "moq_Linux_arm64.tar.gz",
            "moq_Linux_armv6.tar.gz",
            "moq_Linux_x86_64.tar.gz",
            "moq_macOS_all.tar.gz",
            "moq_macOS_arm64.tar.gz",
            "moq_macOS_x86_64.tar.gz",
            "moq_Windows_arm64.tar.gz",
            "moq_Windows_armv6.tar.gz",
            "moq_Windows_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_matryer_moq_moq_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 5),
                (Platform::Win64, 9),
                (Platform::WinArm64, 7),
            ],
            &matryer_moq_moq_names(),
            "moq",
        );
    }

    fn mergestat_mergestat_lite_mergestat_names() -> Vec<&'static str> {
        vec![
            "mergestat-linux-amd64.tar.gz",
            "mergestat-macos-amd64.tar.gz",
        ]
    }

    #[test]
    fn test_mergestat_mergestat_lite_mergestat_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
            ],
            &mergestat_mergestat_lite_mergestat_names(),
            "mergestat",
        );
    }

    fn metalbear_co_mirrord_mirrord_names() -> Vec<&'static str> {
        vec![
            "libmirrord_layer_linux_aarch64.so",
            "libmirrord_layer_linux_x86_64.so",
            "libmirrord_layer_mac_universal.dylib",
            "mirrord.exe",
            "mirrord.exe.sha256",
            "mirrord.msi",
            "mirrord.msi.sha256",
            "mirrord_layer_win.dll",
            "mirrord_layer_win.dll.sha256",
            "mirrord_linux_aarch64",
            "mirrord_linux_aarch64.shasum256",
            "mirrord_linux_aarch64.zip",
            "mirrord_linux_x86_64",
            "mirrord_linux_x86_64.shasum256",
            "mirrord_linux_x86_64.zip",
            "mirrord_mac_universal",
            "mirrord_mac_universal.shasum256",
            "mirrord_mac_universal.zip",
        ]
    }

    #[test]
    fn test_metalbear_co_mirrord_mirrord_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 12),
            ],
            &metalbear_co_mirrord_mirrord_names(),
            "mirrord",
        );
    }

    fn mgechev_revive_revive_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "revive_darwin_amd64.tar.gz",
            "revive_darwin_arm64.tar.gz",
            "revive_linux_386.tar.gz",
            "revive_linux_amd64.tar.gz",
            "revive_linux_arm64.tar.gz",
            "revive_windows_386.tar.gz",
            "revive_windows_amd64.tar.gz",
            "revive_windows_arm64.tar.gz",
        ]
    }

    #[test]
    fn test_mgechev_revive_revive_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 3),
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win32, 6),
                (Platform::Win64, 7),
                (Platform::WinArm64, 8),
            ],
            &mgechev_revive_revive_names(),
            "revive",
        );
    }

    fn mgunyho_tere_tere_names() -> Vec<&'static str> {
        vec![
            "tere-1.6.0-aarch64-unknown-linux-gnu.zip",
            "tere-1.6.0-x86_64-pc-windows-gnu.zip",
            "tere-1.6.0-x86_64-unknown-linux-gnu.zip",
            "tere-1.6.0-x86_64-unknown-linux-musl.zip",
        ]
    }

    #[test]
    fn test_mgunyho_tere_tere_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 0),
                (Platform::Win64, 1),
            ],
            &mgunyho_tere_tere_names(),
            "tere",
        );
    }

    fn michidk_vscli_vscli_names() -> Vec<&'static str> {
        vec![
            "vscli-aarch64-apple-darwin.tar.gz",
            "vscli-arm-unknown-linux-gnueabihf.tar.gz",
            "vscli-i686-pc-windows-msvc.zip",
            "vscli-x86_64-apple-darwin.tar.gz",
            "vscli-x86_64-pc-windows-msvc.zip",
            "vscli-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_michidk_vscli_vscli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::Win64, 4),
            ],
            &michidk_vscli_vscli_names(),
            "vscli",
        );
    }

    fn micro_editor_micro_micro_names() -> Vec<&'static str> {
        vec![
            "micro-2.0.15-freebsd32.tar.gz",
            "micro-2.0.15-freebsd32.tar.gz.sha",
            "micro-2.0.15-freebsd64.tar.gz",
            "micro-2.0.15-freebsd64.tar.gz.sha",
            "micro-2.0.15-illumos64.tar.gz",
            "micro-2.0.15-illumos64.tar.gz.sha",
            "micro-2.0.15-linux-arm.tar.gz",
            "micro-2.0.15-linux-arm.tar.gz.sha",
            "micro-2.0.15-linux-arm64.tar.gz",
            "micro-2.0.15-linux-arm64.tar.gz.sha",
            "micro-2.0.15-linux32.tar.gz",
            "micro-2.0.15-linux32.tar.gz.sha",
            "micro-2.0.15-linux64-static.tar.gz",
            "micro-2.0.15-linux64-static.tar.gz.sha",
            "micro-2.0.15-linux64.tar.gz",
            "micro-2.0.15-linux64.tar.gz.sha",
            "micro-2.0.15-macos-arm64.tar.gz",
            "micro-2.0.15-macos-arm64.tar.gz.sha",
            "micro-2.0.15-netbsd32.tar.gz",
            "micro-2.0.15-netbsd32.tar.gz.sha",
            "micro-2.0.15-netbsd64.tar.gz",
            "micro-2.0.15-netbsd64.tar.gz.sha",
            "micro-2.0.15-openbsd32.tar.gz",
            "micro-2.0.15-openbsd32.tar.gz.sha",
            "micro-2.0.15-openbsd64.tar.gz",
            "micro-2.0.15-openbsd64.tar.gz.sha",
            "micro-2.0.15-osx.tar.gz",
            "micro-2.0.15-osx.tar.gz.sha",
            "micro-2.0.15-solaris64.tar.gz",
            "micro-2.0.15-solaris64.tar.gz.sha",
            "micro-2.0.15-win-arm64.zip",
            "micro-2.0.15-win-arm64.zip.sha",
            "micro-2.0.15-win32.zip",
            "micro-2.0.15-win32.zip.sha",
            "micro-2.0.15-win64.zip",
            "micro-2.0.15-win64.zip.sha",
        ]
    }

    #[test]
    fn test_micro_editor_micro_micro_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 12),
                (Platform::LinuxAarch64, 8),
                (Platform::Osx64, 26),
                (Platform::OsxArm64, 16),
                (Platform::Win32, 34),
                (Platform::Win64, 34),
                (Platform::WinArm64, 34),
            ],
            &micro_editor_micro_micro_names(),
            "micro",
        );
    }

    fn microsoft_edit_edit_names() -> Vec<&'static str> {
        vec![
            "edit-1.2.0-aarch64-linux-gnu.tar.zst",
            "edit-1.2.0-x86_64-linux-gnu.tar.zst",
            "edit-1.2.1-aarch64-windows.zip",
            "edit-1.2.1-x86_64-windows.zip",
        ]
    }

    #[test]
    fn test_microsoft_edit_edit_names() {
        platform_match_test(
            &[
                (Platform::Win64, 3),
                (Platform::WinArm64, 2),
            ],
            &microsoft_edit_edit_names(),
            "edit",
        );
    }

    fn microsoft_kiota_kiota_names() -> Vec<&'static str> {
        vec![
            "kiota-1.30.100000001.vsix",
            "linux-arm64.zip",
            "linux-x64.zip",
            "Microsoft.OpenApi.Kiota.1.30.0.nupkg",
            "Microsoft.OpenApi.Kiota.1.30.0.snupkg",
            "Microsoft.OpenApi.Kiota.Builder.1.30.0.nupkg",
            "Microsoft.OpenApi.Kiota.Builder.1.30.0.snupkg",
            "osx-arm64.zip",
            "osx-x64.zip",
            "win-arm64.zip",
            "win-x64.zip",
            "win-x86.zip",
        ]
    }

    #[test]
    fn test_microsoft_kiota_kiota_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 8),
                (Platform::OsxArm64, 7),
                (Platform::Win64, 10),
                (Platform::WinArm64, 9),
            ],
            &microsoft_kiota_kiota_names(),
            "kiota",
        );
    }

    fn mike_engel_jwt_cli_jwt_names() -> Vec<&'static str> {
        vec![
            "jwt-linux-musl.sha256",
            "jwt-linux-musl.tar.gz",
            "jwt-linux.sha256",
            "jwt-linux.tar.gz",
            "jwt-macOS.sha256",
            "jwt-macOS.tar.gz",
            "jwt-windows.sha256",
            "jwt-windows.tar.gz",
        ]
    }

    #[test]
    fn test_mike_engel_jwt_cli_jwt_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 5),
                (Platform::Win32, 7),
                (Platform::Win64, 7),
                (Platform::WinArm64, 7),
            ],
            &mike_engel_jwt_cli_jwt_names(),
            "jwt",
        );
    }

    fn mintoolkit_mint_dist_names() -> Vec<&'static str> {
        vec![
            "dist_linux.tar.gz",
            "dist_linux_arm.tar.gz",
            "dist_linux_arm64.tar.gz",
            "dist_mac.zip",
            "dist_mac_m1.zip",
        ]
    }

    #[test]
    fn test_mintoolkit_mint_dist_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 0),
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 3),
            ],
            &mintoolkit_mint_dist_names(),
            "dist",
        );
    }

    fn mitchellh_golicense_golicense_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "golicense_0.2.0_freebsd_i386.tar.gz",
            "golicense_0.2.0_freebsd_x86_64.tar.gz",
            "golicense_0.2.0_linux_i386.tar.gz",
            "golicense_0.2.0_linux_x86_64.tar.gz",
            "golicense_0.2.0_macos_i386.tar.gz",
            "golicense_0.2.0_macos_x86_64.tar.gz",
            "golicense_0.2.0_windows_i386.tar.gz",
            "golicense_0.2.0_windows_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_mitchellh_golicense_golicense_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::Osx64, 6),
                (Platform::Win64, 8),
            ],
            &mitchellh_golicense_golicense_names(),
            "golicense",
        );
    }

    fn mozilla_grcov_grcov_names() -> Vec<&'static str> {
        vec![
            "checksums.sha256",
            "grcov-aarch64-apple-darwin.tar.bz2",
            "grcov-aarch64-pc-windows-msvc.zip",
            "grcov-aarch64-unknown-linux-gnu.tar.bz2",
            "grcov-aarch64-unknown-linux-musl.tar.bz2",
            "grcov-x86_64-apple-darwin.tar.bz2",
            "grcov-x86_64-pc-windows-msvc.zip",
            "grcov-x86_64-unknown-linux-gnu-tc.tar.bz2",
            "grcov-x86_64-unknown-linux-gnu.tar.bz2",
            "grcov-x86_64-unknown-linux-musl.tar.bz2",
        ]
    }

    #[test]
    fn test_mozilla_grcov_grcov_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 6),
                (Platform::WinArm64, 2),
            ],
            &mozilla_grcov_grcov_names(),
            "grcov",
        );
    }

    fn mozilla_sccache_sccache_names() -> Vec<&'static str> {
        vec![
            "sccache-dist-v0.14.0-x86_64-unknown-linux-musl.tar.gz",
            "sccache-dist-v0.14.0-x86_64-unknown-linux-musl.tar.gz.sha256",
            "sccache-v0.14.0-aarch64-apple-darwin.tar.gz",
            "sccache-v0.14.0-aarch64-apple-darwin.tar.gz.sha256",
            "sccache-v0.14.0-aarch64-pc-windows-msvc.tar.gz",
            "sccache-v0.14.0-aarch64-pc-windows-msvc.tar.gz.sha256",
            "sccache-v0.14.0-aarch64-pc-windows-msvc.zip",
            "sccache-v0.14.0-aarch64-pc-windows-msvc.zip.sha256",
            "sccache-v0.14.0-aarch64-unknown-linux-musl.tar.gz",
            "sccache-v0.14.0-aarch64-unknown-linux-musl.tar.gz.sha256",
            "sccache-v0.14.0-armv7-unknown-linux-musleabi.tar.gz",
            "sccache-v0.14.0-armv7-unknown-linux-musleabi.tar.gz.sha256",
            "sccache-v0.14.0-i686-unknown-linux-musl.tar.gz",
            "sccache-v0.14.0-i686-unknown-linux-musl.tar.gz.sha256",
            "sccache-v0.14.0-riscv64gc-unknown-linux-musl.tar.gz",
            "sccache-v0.14.0-riscv64gc-unknown-linux-musl.tar.gz.sha256",
            "sccache-v0.14.0-s390x-unknown-linux-gnu.tar.gz",
            "sccache-v0.14.0-s390x-unknown-linux-gnu.tar.gz.sha256",
            "sccache-v0.14.0-s390x-unknown-linux-musl.tar.gz",
            "sccache-v0.14.0-s390x-unknown-linux-musl.tar.gz.sha256",
            "sccache-v0.14.0-x86_64-apple-darwin.tar.gz",
            "sccache-v0.14.0-x86_64-apple-darwin.tar.gz.sha256",
            "sccache-v0.14.0-x86_64-pc-windows-msvc.tar.gz",
            "sccache-v0.14.0-x86_64-pc-windows-msvc.tar.gz.sha256",
            "sccache-v0.14.0-x86_64-pc-windows-msvc.zip",
            "sccache-v0.14.0-x86_64-pc-windows-msvc.zip.sha256",
            "sccache-v0.14.0-x86_64-unknown-linux-musl.tar.gz",
            "sccache-v0.14.0-x86_64-unknown-linux-musl.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_mozilla_sccache_sccache_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 26),
                (Platform::Osx64, 20),
                (Platform::OsxArm64, 2),
            ],
            &mozilla_sccache_sccache_names(),
            "sccache",
        );
    }

    fn mrjackwills_oxker_oxker_names() -> Vec<&'static str> {
        vec![
            "oxker_apple_darwin_aarch64.tar.gz",
            "oxker_linux_aarch64.tar.gz",
            "oxker_linux_armv6.tar.gz",
            "oxker_linux_x86_64.tar.gz",
            "oxker_windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_mrjackwills_oxker_oxker_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 4),
            ],
            &mrjackwills_oxker_oxker_names(),
            "oxker",
        );
    }

    fn ms_jpq_sad_zip_names() -> Vec<&'static str> {
        vec![
            "aarch64-apple-darwin.zip",
            "aarch64-pc-windows-msvc.zip",
            "aarch64-unknown-linux-gnu.deb",
            "aarch64-unknown-linux-gnu.zip",
            "aarch64-unknown-linux-musl.deb",
            "aarch64-unknown-linux-musl.zip",
            "x86_64-apple-darwin.zip",
            "x86_64-pc-windows-gnu.zip",
            "x86_64-pc-windows-msvc.zip",
            "x86_64-unknown-linux-gnu.deb",
            "x86_64-unknown-linux-gnu.zip",
            "x86_64-unknown-linux-musl.deb",
            "x86_64-unknown-linux-musl.zip",
        ]
    }

    #[test]
    fn test_ms_jpq_sad_zip_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 12),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 7),
            ],
            &ms_jpq_sad_zip_names(),
            "zip",
        );
    }

    fn mstange_samply_samply_names() -> Vec<&'static str> {
        vec![
            "dist-manifest.json",
            "samply-aarch64-apple-darwin.tar.xz",
            "samply-aarch64-apple-darwin.tar.xz.sha256",
            "samply-aarch64-unknown-linux-gnu.tar.xz",
            "samply-aarch64-unknown-linux-gnu.tar.xz.sha256",
            "samply-installer.ps1",
            "samply-installer.sh",
            "samply-x86_64-apple-darwin.tar.xz",
            "samply-x86_64-apple-darwin.tar.xz.sha256",
            "samply-x86_64-pc-windows-msvc.zip",
            "samply-x86_64-pc-windows-msvc.zip.sha256",
            "samply-x86_64-unknown-linux-gnu.tar.xz",
            "samply-x86_64-unknown-linux-gnu.tar.xz.sha256",
            "samply-x86_64-unknown-linux-musl.tar.xz",
            "samply-x86_64-unknown-linux-musl.tar.xz.sha256",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_mstange_samply_samply_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 13),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 9),
            ],
            &mstange_samply_samply_names(),
            "samply",
        );
    }

    fn muquit_mailsend_go_mailsend_go_names() -> Vec<&'static str> {
        vec![
            "mailsend-go-v1.0.11-b2-checksums.txt",
            "mailsend-go-v1.0.11-b2-darwin-amd64.d.tar.gz",
            "mailsend-go-v1.0.11-b2-darwin-arm64.d.tar.gz",
            "mailsend-go-v1.0.11-b2-linux-amd64.d.tar.gz",
            "mailsend-go-v1.0.11-b2-linux-arm.d.tar.gz",
            "mailsend-go-v1.0.11-b2-linux-arm64.d.tar.gz",
            "mailsend-go-v1.0.11-b2-raspberry-pi-jessie.d.tar.gz",
            "mailsend-go-v1.0.11-b2-raspberry-pi.d.tar.gz",
            "mailsend-go-v1.0.11-b2-windows-386.d.zip",
            "mailsend-go-v1.0.11-b2-windows-amd64.d.zip",
        ]
    }

    #[test]
    fn test_muquit_mailsend_go_mailsend_go_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win32, 8),
                (Platform::Win64, 9),
            ],
            &muquit_mailsend_go_mailsend_go_names(),
            "mailsend-go",
        );
    }

    fn natecraddock_zf_zf_names() -> Vec<&'static str> {
        vec![
            "zf-0.10.2-aarch64-linux.tar.xz",
            "zf-0.10.2-aarch64-macos.tar.xz",
            "zf-0.10.2-x86_64-linux.tar.xz",
            "zf-0.10.2-x86_64-macos.tar.xz",
        ]
    }

    #[test]
    fn test_natecraddock_zf_zf_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 1),
            ],
            &natecraddock_zf_zf_names(),
            "zf",
        );
    }

    fn neilotoole_sq_sq_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "sq-0.50.0-linux-amd64.tar.gz",
            "sq-0.50.0-linux-arm64.tar.gz",
            "sq-0.50.0-macos-amd64.tar.gz",
            "sq-0.50.0-macos-arm64.tar.gz",
            "sq-0.50.0-windows-amd64.zip",
            "sq_0.50.0_linux_amd64.apk",
            "sq_0.50.0_linux_amd64.deb",
            "sq_0.50.0_linux_amd64.pkg.tar.zst",
            "sq_0.50.0_linux_amd64.rpm",
            "sq_0.50.0_linux_arm64.apk",
            "sq_0.50.0_linux_arm64.deb",
            "sq_0.50.0_linux_arm64.pkg.tar.zst",
            "sq_0.50.0_linux_arm64.rpm",
        ]
    }

    #[test]
    fn test_neilotoole_sq_sq_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 5),
            ],
            &neilotoole_sq_sq_names(),
            "sq",
        );
    }

    fn nektro_zigmod_zigmod_names() -> Vec<&'static str> {
        vec![
            "zigmod-aarch64-linux",
            "zigmod-aarch64-macos",
            "zigmod-aarch64-windows.exe",
            "zigmod-aarch64-windows.pdb",
            "zigmod-loongarch64-linux",
            "zigmod-mips64el-linux",
            "zigmod-powerpc64le-linux",
            "zigmod-riscv64-linux",
            "zigmod-s390x-linux",
            "zigmod-x86_64-linux",
            "zigmod-x86_64-macos",
            "zigmod-x86_64-windows.exe",
            "zigmod-x86_64-windows.pdb",
        ]
    }

    #[test]
    fn test_nektro_zigmod_zigmod_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 1),
            ],
            &nektro_zigmod_zigmod_names(),
            "zigmod",
        );
    }

    fn neondatabase_neonctl_neonctl_names() -> Vec<&'static str> {
        vec![
            "neonctl-linux-arm64",
            "neonctl-linux-x64",
            "neonctl-macos-x64",
            "neonctl-win-x64.exe",
        ]
    }

    #[test]
    fn test_neondatabase_neonctl_neonctl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 2),
            ],
            &neondatabase_neonctl_neonctl_names(),
            "neonctl",
        );
    }

    fn neovim_neovim_nvim_names() -> Vec<&'static str> {
        vec![
            "nvim-linux-arm64.appimage",
            "nvim-linux-arm64.appimage.zsync",
            "nvim-linux-arm64.tar.gz",
            "nvim-linux-x86_64.appimage",
            "nvim-linux-x86_64.appimage.zsync",
            "nvim-linux-x86_64.tar.gz",
            "nvim-macos-arm64.tar.gz",
            "nvim-macos-x86_64.tar.gz",
            "nvim-win-arm64.msi",
            "nvim-win-arm64.zip",
            "nvim-win64.msi",
            "nvim-win64.zip",
        ]
    }

    #[test]
    fn test_neovim_neovim_nvim_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 6),
                (Platform::Win32, 11),
                (Platform::Win64, 11),
                (Platform::WinArm64, 11),
            ],
            &neovim_neovim_nvim_names(),
            "nvim",
        );
    }

    fn neovim_neovim_releases_nvim_names() -> Vec<&'static str> {
        vec![
            "nvim-linux-x86_64.appimage",
            "nvim-linux-x86_64.appimage.zsync",
            "nvim-linux-x86_64.deb",
            "nvim-linux-x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_neovim_neovim_releases_nvim_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
            ],
            &neovim_neovim_releases_nvim_names(),
            "nvim",
        );
    }

    fn nextest_rs_nextest_nextest_names() -> Vec<&'static str> {
        vec![
            "cargo-nextest-0.9.129-aarch64-pc-windows-msvc.b2",
            "cargo-nextest-0.9.129-aarch64-pc-windows-msvc.sha256",
            "cargo-nextest-0.9.129-aarch64-pc-windows-msvc.tar.gz",
            "cargo-nextest-0.9.129-aarch64-pc-windows-msvc.zip",
            "cargo-nextest-0.9.129-aarch64-unknown-linux-gnu.b2",
            "cargo-nextest-0.9.129-aarch64-unknown-linux-gnu.sha256",
            "cargo-nextest-0.9.129-aarch64-unknown-linux-gnu.tar.gz",
            "cargo-nextest-0.9.129-aarch64-unknown-linux-musl.b2",
            "cargo-nextest-0.9.129-aarch64-unknown-linux-musl.sha256",
            "cargo-nextest-0.9.129-aarch64-unknown-linux-musl.tar.gz",
            "cargo-nextest-0.9.129-i686-pc-windows-msvc.b2",
            "cargo-nextest-0.9.129-i686-pc-windows-msvc.sha256",
            "cargo-nextest-0.9.129-i686-pc-windows-msvc.tar.gz",
            "cargo-nextest-0.9.129-i686-pc-windows-msvc.zip",
            "cargo-nextest-0.9.129-universal-apple-darwin.b2",
            "cargo-nextest-0.9.129-universal-apple-darwin.sha256",
            "cargo-nextest-0.9.129-universal-apple-darwin.tar.gz",
            "cargo-nextest-0.9.129-x86_64-pc-windows-msvc.b2",
            "cargo-nextest-0.9.129-x86_64-pc-windows-msvc.sha256",
            "cargo-nextest-0.9.129-x86_64-pc-windows-msvc.tar.gz",
            "cargo-nextest-0.9.129-x86_64-pc-windows-msvc.zip",
            "cargo-nextest-0.9.129-x86_64-unknown-freebsd.b2",
            "cargo-nextest-0.9.129-x86_64-unknown-freebsd.sha256",
            "cargo-nextest-0.9.129-x86_64-unknown-freebsd.tar.gz",
            "cargo-nextest-0.9.129-x86_64-unknown-illumos.b2",
            "cargo-nextest-0.9.129-x86_64-unknown-illumos.sha256",
            "cargo-nextest-0.9.129-x86_64-unknown-illumos.tar.gz",
            "cargo-nextest-0.9.129-x86_64-unknown-linux-gnu.b2",
            "cargo-nextest-0.9.129-x86_64-unknown-linux-gnu.sha256",
            "cargo-nextest-0.9.129-x86_64-unknown-linux-gnu.tar.gz",
            "cargo-nextest-0.9.129-x86_64-unknown-linux-musl.b2",
            "cargo-nextest-0.9.129-x86_64-unknown-linux-musl.sha256",
            "cargo-nextest-0.9.129-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_nextest_rs_nextest_nextest_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 32),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 16),
                (Platform::OsxArm64, 16),
                (Platform::Win64, 19),
            ],
            &nextest_rs_nextest_nextest_names(),
            "nextest",
        );
    }

    fn nickel_lang_nickel_nickel_names() -> Vec<&'static str> {
        vec![
            "libnickel_lang-arm64-linux.a",
            "libnickel_lang-arm64-linux.so",
            "libnickel_lang-arm64-macos.a",
            "libnickel_lang-arm64-macos.dylib",
            "libnickel_lang-x86_64-linux.a",
            "libnickel_lang-x86_64-linux.so",
            "libnickel_lang-x86_64-windows-mingw.a",
            "libnickel_lang-x86_64-windows-mingw.dll",
            "nickel-arm64-docker-image.tar.gz",
            "nickel-arm64-linux",
            "nickel-arm64-macos",
            "nickel-pkg-arm64-linux",
            "nickel-pkg-x86_64-linux",
            "nickel-pkg-x86_64-windows.exe",
            "nickel-x86_64-docker-image.tar.gz",
            "nickel-x86_64-linux",
            "nickel-x86_64-windows.exe",
            "nickel_lang.h",
            "nls-arm64-linux",
            "nls-arm64-macos",
            "nls-x86_64-linux",
            "nls-x86_64-windows.exe",
        ]
    }

    #[test]
    fn test_nickel_lang_nickel_nickel_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 15),
                (Platform::LinuxAarch64, 9),
                (Platform::OsxArm64, 10),
            ],
            &nickel_lang_nickel_nickel_names(),
            "nickel",
        );
    }

    fn nil0x42_dnsanity_dnsanity_names() -> Vec<&'static str> {
        vec![
            "dnsanity-linux-x64-v1.4.1",
            "dnsanity-mac-arm64-v1.4.1",
            "dnsanity-mac-x64-v1.4.1",
        ]
    }

    #[test]
    fn test_nil0x42_dnsanity_dnsanity_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
            ],
            &nil0x42_dnsanity_dnsanity_names(),
            "dnsanity",
        );
    }

    fn ninja_build_ninja_ninja_names() -> Vec<&'static str> {
        vec![
            "ninja-linux-aarch64.zip",
            "ninja-linux.zip",
            "ninja-mac.zip",
            "ninja-win.zip",
            "ninja-winarm64.zip",
        ]
    }

    #[test]
    fn test_ninja_build_ninja_ninja_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 1),
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 2),
                (Platform::Win32, 3),
                (Platform::Win64, 3),
                (Platform::WinArm64, 4),
            ],
            &ninja_build_ninja_ninja_names(),
            "ninja",
        );
    }

    fn numtide_treefmt_treefmt_names() -> Vec<&'static str> {
        vec![
            "treefmt_2.4.1_checksums.txt",
            "treefmt_2.4.1_darwin_amd64.tar.gz",
            "treefmt_2.4.1_darwin_arm64.tar.gz",
            "treefmt_2.4.1_linux_386.tar.gz",
            "treefmt_2.4.1_linux_amd64.tar.gz",
            "treefmt_2.4.1_linux_arm64.tar.gz",
        ]
    }

    #[test]
    fn test_numtide_treefmt_treefmt_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 3),
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
            ],
            &numtide_treefmt_treefmt_names(),
            "treefmt",
        );
    }

    fn o2sh_onefetch_onefetch_names() -> Vec<&'static str> {
        vec![
            "onefetch-linux.tar.gz",
            "onefetch-mac.tar.gz",
            "onefetch-setup.exe",
            "onefetch-win.tar.gz",
            "onefetch_amd64.deb",
        ]
    }

    #[test]
    fn test_o2sh_onefetch_onefetch_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
                (Platform::Win32, 3),
                (Platform::Win64, 3),
                (Platform::WinArm64, 3),
            ],
            &o2sh_onefetch_onefetch_names(),
            "onefetch",
        );
    }

    fn oberblastmeister_trashy_trash_names() -> Vec<&'static str> {
        vec![
            "trash-x86_64-pc-windows-msvc.exe",
            "trash-x86_64-unknown-linux-gnu.tar.gz",
        ]
    }

    #[test]
    fn test_oberblastmeister_trashy_trash_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
            ],
            &oberblastmeister_trashy_trash_names(),
            "trash",
        );
    }

    fn ogham_dog_dog_names() -> Vec<&'static str> {
        vec![
            "dog-v0.1.0-x86_64-apple-darwin.zip",
            "dog-v0.1.0-x86_64-apple-darwin.zip.minisig",
            "dog-v0.1.0-x86_64-pc-windows-msvc.zip",
            "dog-v0.1.0-x86_64-pc-windows-msvc.zip.minisig",
            "dog-v0.1.0-x86_64-unknown-linux-gnu.zip",
            "dog-v0.1.0-x86_64-unknown-linux-gnu.zip.minisig",
            "SHA256SUMS",
        ]
    }

    #[test]
    fn test_ogham_dog_dog_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::Osx64, 0),
                (Platform::Win64, 2),
            ],
            &ogham_dog_dog_names(),
            "dog",
        );
    }

    fn ogham_exa_exa_names() -> Vec<&'static str> {
        vec![
            "exa-accoutrements-v0.10.1.zip",
            "exa-linux-armv7-v0.10.1.zip",
            "exa-linux-x86_64-musl-v0.10.1.zip",
            "exa-linux-x86_64-v0.10.1.zip",
            "exa-macos-x86_64-v0.10.1.zip",
            "exa-vendored-source-v0.10.1.zip",
        ]
    }

    #[test]
    fn test_ogham_exa_exa_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 4),
            ],
            &ogham_exa_exa_names(),
            "exa",
        );
    }

    fn okta_okta_aws_cli_okta_aws_cli_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "okta-aws-cli_2.6.0_darwin_amd64.tar.gz",
            "okta-aws-cli_2.6.0_darwin_amd64.zip",
            "okta-aws-cli_2.6.0_darwin_arm64.tar.gz",
            "okta-aws-cli_2.6.0_darwin_arm64.zip",
            "okta-aws-cli_2.6.0_freebsd_386.tar.gz",
            "okta-aws-cli_2.6.0_freebsd_386.zip",
            "okta-aws-cli_2.6.0_freebsd_amd64.tar.gz",
            "okta-aws-cli_2.6.0_freebsd_amd64.zip",
            "okta-aws-cli_2.6.0_freebsd_arm64.tar.gz",
            "okta-aws-cli_2.6.0_freebsd_arm64.zip",
            "okta-aws-cli_2.6.0_linux_386.tar.gz",
            "okta-aws-cli_2.6.0_linux_386.zip",
            "okta-aws-cli_2.6.0_linux_amd64.tar.gz",
            "okta-aws-cli_2.6.0_linux_amd64.zip",
            "okta-aws-cli_2.6.0_linux_arm64.tar.gz",
            "okta-aws-cli_2.6.0_linux_arm64.zip",
            "okta-aws-cli_2.6.0_windows_386.tar.gz",
            "okta-aws-cli_2.6.0_windows_386.zip",
            "okta-aws-cli_2.6.0_windows_arm64.tar.gz",
            "okta-aws-cli_2.6.0_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_okta_okta_aws_cli_okta_aws_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 11),
                (Platform::Linux64, 13),
                (Platform::LinuxAarch64, 15),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 3),
                (Platform::WinArm64, 19),
            ],
            &okta_okta_aws_cli_okta_aws_cli_names(),
            "okta-aws-cli",
        );
    }

    fn openai_codex_codex_names() -> Vec<&'static str> {
        vec![
            "codex",
            "codex-aarch64-apple-darwin.dmg",
            "codex-aarch64-apple-darwin.tar.gz",
            "codex-aarch64-apple-darwin.zst",
            "codex-aarch64-pc-windows-msvc.exe",
            "codex-aarch64-pc-windows-msvc.exe.tar.gz",
            "codex-aarch64-pc-windows-msvc.exe.zip",
            "codex-aarch64-pc-windows-msvc.exe.zst",
            "codex-aarch64-unknown-linux-gnu.sigstore",
            "codex-aarch64-unknown-linux-gnu.tar.gz",
            "codex-aarch64-unknown-linux-gnu.zst",
            "codex-aarch64-unknown-linux-musl.sigstore",
            "codex-aarch64-unknown-linux-musl.tar.gz",
            "codex-aarch64-unknown-linux-musl.zst",
            "codex-command-runner",
            "codex-command-runner-aarch64-pc-windows-msvc.exe",
            "codex-command-runner-aarch64-pc-windows-msvc.exe.tar.gz",
            "codex-command-runner-aarch64-pc-windows-msvc.exe.zip",
            "codex-command-runner-aarch64-pc-windows-msvc.exe.zst",
            "codex-command-runner-x86_64-pc-windows-msvc.exe",
            "codex-command-runner-x86_64-pc-windows-msvc.exe.tar.gz",
            "codex-command-runner-x86_64-pc-windows-msvc.exe.zip",
            "codex-command-runner-x86_64-pc-windows-msvc.exe.zst",
            "codex-npm-0.107.0.tgz",
            "codex-npm-darwin-arm64-0.107.0.tgz",
            "codex-npm-darwin-x64-0.107.0.tgz",
            "codex-npm-linux-arm64-0.107.0.tgz",
            "codex-npm-linux-x64-0.107.0.tgz",
            "codex-npm-win32-arm64-0.107.0.tgz",
            "codex-npm-win32-x64-0.107.0.tgz",
            "codex-responses-api-proxy",
            "codex-responses-api-proxy-aarch64-apple-darwin.tar.gz",
            "codex-responses-api-proxy-aarch64-apple-darwin.zst",
            "codex-responses-api-proxy-aarch64-pc-windows-msvc.exe",
            "codex-responses-api-proxy-aarch64-pc-windows-msvc.exe.tar.gz",
            "codex-responses-api-proxy-aarch64-pc-windows-msvc.exe.zip",
            "codex-responses-api-proxy-aarch64-pc-windows-msvc.exe.zst",
            "codex-responses-api-proxy-aarch64-unknown-linux-gnu.sigstore",
            "codex-responses-api-proxy-aarch64-unknown-linux-gnu.tar.gz",
            "codex-responses-api-proxy-aarch64-unknown-linux-gnu.zst",
            "codex-responses-api-proxy-aarch64-unknown-linux-musl.sigstore",
            "codex-responses-api-proxy-aarch64-unknown-linux-musl.tar.gz",
            "codex-responses-api-proxy-aarch64-unknown-linux-musl.zst",
            "codex-responses-api-proxy-npm-0.107.0.tgz",
            "codex-responses-api-proxy-x86_64-apple-darwin.tar.gz",
            "codex-responses-api-proxy-x86_64-apple-darwin.zst",
            "codex-responses-api-proxy-x86_64-pc-windows-msvc.exe",
            "codex-responses-api-proxy-x86_64-pc-windows-msvc.exe.tar.gz",
            "codex-responses-api-proxy-x86_64-pc-windows-msvc.exe.zip",
            "codex-responses-api-proxy-x86_64-pc-windows-msvc.exe.zst",
            "codex-responses-api-proxy-x86_64-unknown-linux-gnu.sigstore",
            "codex-responses-api-proxy-x86_64-unknown-linux-gnu.tar.gz",
            "codex-responses-api-proxy-x86_64-unknown-linux-gnu.zst",
            "codex-responses-api-proxy-x86_64-unknown-linux-musl.sigstore",
            "codex-responses-api-proxy-x86_64-unknown-linux-musl.tar.gz",
            "codex-responses-api-proxy-x86_64-unknown-linux-musl.zst",
            "codex-sdk-npm-0.107.0.tgz",
            "codex-shell-tool-mcp-npm-0.107.0.tgz",
            "codex-windows-sandbox-setup",
            "codex-windows-sandbox-setup-aarch64-pc-windows-msvc.exe",
            "codex-windows-sandbox-setup-aarch64-pc-windows-msvc.exe.tar.gz",
            "codex-windows-sandbox-setup-aarch64-pc-windows-msvc.exe.zip",
            "codex-windows-sandbox-setup-aarch64-pc-windows-msvc.exe.zst",
            "codex-windows-sandbox-setup-x86_64-pc-windows-msvc.exe",
            "codex-windows-sandbox-setup-x86_64-pc-windows-msvc.exe.tar.gz",
            "codex-windows-sandbox-setup-x86_64-pc-windows-msvc.exe.zip",
            "codex-windows-sandbox-setup-x86_64-pc-windows-msvc.exe.zst",
            "codex-x86_64-apple-darwin.dmg",
            "codex-x86_64-apple-darwin.tar.gz",
            "codex-x86_64-apple-darwin.zst",
            "codex-x86_64-pc-windows-msvc.exe",
            "codex-x86_64-pc-windows-msvc.exe.tar.gz",
            "codex-x86_64-pc-windows-msvc.exe.zip",
            "codex-x86_64-pc-windows-msvc.exe.zst",
            "codex-x86_64-unknown-linux-gnu.sigstore",
            "codex-x86_64-unknown-linux-gnu.tar.gz",
            "codex-x86_64-unknown-linux-gnu.zst",
            "codex-x86_64-unknown-linux-musl.sigstore",
            "codex-x86_64-unknown-linux-musl.tar.gz",
            "codex-x86_64-unknown-linux-musl.zst",
            "config-schema.json",
            "install.sh",
        ]
    }

    #[test]
    fn test_openai_codex_codex_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 79),
                (Platform::LinuxAarch64, 13),
                (Platform::Osx64, 69),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 73),
                (Platform::WinArm64, 7),
            ],
            &openai_codex_codex_names(),
            "codex",
        );
    }

    fn opengrep_opengrep_opengrep_names() -> Vec<&'static str> {
        vec![
            "opengrep-core_linux_aarch64.tar.gz",
            "opengrep-core_linux_x86.tar.gz",
            "opengrep-core_osx_aarch64.tar.gz",
            "opengrep-core_osx_x86.tar.gz",
            "opengrep-core_windows_x86.zip",
            "opengrep_manylinux_aarch64",
            "opengrep_manylinux_aarch64.cert",
            "opengrep_manylinux_aarch64.sig",
            "opengrep_manylinux_x86",
            "opengrep_manylinux_x86.cert",
            "opengrep_manylinux_x86.sig",
            "opengrep_musllinux_aarch64",
            "opengrep_musllinux_aarch64.cert",
            "opengrep_musllinux_aarch64.sig",
            "opengrep_musllinux_x86",
            "opengrep_musllinux_x86.cert",
            "opengrep_musllinux_x86.sig",
            "opengrep_osx_arm64",
            "opengrep_osx_arm64.cert",
            "opengrep_osx_arm64.sig",
            "opengrep_osx_x86",
            "opengrep_osx_x86.cert",
            "opengrep_osx_x86.sig",
            "opengrep_windows_x86.exe",
            "opengrep_windows_x86.exe.cert",
            "opengrep_windows_x86.exe.sig",
        ]
    }

    #[test]
    fn test_opengrep_opengrep_opengrep_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 20),
                (Platform::OsxArm64, 17),
            ],
            &opengrep_opengrep_opengrep_names(),
            "opengrep",
        );
    }

    fn openshift_pipelines_pipelines_as_code_tkn_pac_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "release.k8s.yaml",
            "release.yaml",
            "tkn-pac-0.42.0_linux-arm64.deb",
            "tkn-pac-0.42.0_linux-arm64.rpm",
            "tkn-pac-0.42.0_linux-ppc64le.deb",
            "tkn-pac-0.42.0_linux-ppc64le.rpm",
            "tkn-pac-0.42.0_linux-s390x.deb",
            "tkn-pac-0.42.0_linux-s390x.rpm",
            "tkn-pac-0.42.0_linux-x86_64.deb",
            "tkn-pac-0.42.0_linux-x86_64.rpm",
            "tkn-pac_0.42.0_darwin_all.zip",
            "tkn-pac_0.42.0_linux_arm64.tar.gz",
            "tkn-pac_0.42.0_linux_ppc64le.tar.gz",
            "tkn-pac_0.42.0_linux_s390x.tar.gz",
            "tkn-pac_0.42.0_linux_x86_64.tar.gz",
            "tkn-pac_0.42.0_windows_arm64.zip",
            "tkn-pac_0.42.0_windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_openshift_pipelines_pipelines_as_code_tkn_pac_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 15),
                (Platform::LinuxAarch64, 12),
                (Platform::Osx64, 11),
                (Platform::OsxArm64, 11),
                (Platform::Win64, 17),
                (Platform::WinArm64, 16),
            ],
            &openshift_pipelines_pipelines_as_code_tkn_pac_names(),
            "tkn-pac",
        );
    }

    fn oppiliappan_dijo_dijo_names() -> Vec<&'static str> {
        vec![
            "dijo-x86_64-apple",
            "dijo-x86_64-linux",
            "dijo-x86_64-windows.exe",
        ]
    }

    #[test]
    fn test_oppiliappan_dijo_dijo_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 0),
            ],
            &oppiliappan_dijo_dijo_names(),
            "dijo",
        );
    }

    fn ouch_org_ouch_ouch_names() -> Vec<&'static str> {
        vec![
            "ouch-aarch64-pc-windows-msvc.zip",
            "ouch-aarch64-unknown-linux-gnu.tar.gz",
            "ouch-aarch64-unknown-linux-musl.tar.gz",
            "ouch-armv7-unknown-linux-gnueabihf.tar.gz",
            "ouch-armv7-unknown-linux-musleabihf.tar.gz",
            "ouch-x86_64-apple-darwin.tar.gz",
            "ouch-x86_64-pc-windows-gnu.zip",
            "ouch-x86_64-pc-windows-msvc.zip",
            "ouch-x86_64-unknown-linux-gnu.tar.gz",
            "ouch-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_ouch_org_ouch_ouch_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 5),
                (Platform::Win64, 7),
                (Platform::WinArm64, 0),
            ],
            &ouch_org_ouch_ouch_names(),
            "ouch",
        );
    }

    fn out_of_cheese_error_the_way_the_way_names() -> Vec<&'static str> {
        vec![
            "the-way-linux.sha256",
            "the-way-linux.tar.gz",
            "the-way-macos.sha256",
            "the-way-macos.tar.gz",
        ]
    }

    #[test]
    fn test_out_of_cheese_error_the_way_the_way_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 3),
            ],
            &out_of_cheese_error_the_way_the_way_names(),
            "the-way",
        );
    }

    fn oven_sh_bun_bun_names() -> Vec<&'static str> {
        vec![
            "bun-darwin-aarch64-profile.zip",
            "bun-darwin-aarch64.zip",
            "bun-darwin-x64-baseline-profile.zip",
            "bun-darwin-x64-baseline.zip",
            "bun-darwin-x64-profile.zip",
            "bun-darwin-x64.zip",
            "bun-linux-aarch64-musl-profile.zip",
            "bun-linux-aarch64-musl.zip",
            "bun-linux-aarch64-profile.zip",
            "bun-linux-aarch64.zip",
            "bun-linux-x64-baseline-profile.zip",
            "bun-linux-x64-baseline.zip",
            "bun-linux-x64-musl-baseline-profile.zip",
            "bun-linux-x64-musl-baseline.zip",
            "bun-linux-x64-musl-profile.zip",
            "bun-linux-x64-musl.zip",
            "bun-linux-x64-profile.zip",
            "bun-linux-x64.zip",
            "bun-windows-aarch64-profile.zip",
            "bun-windows-aarch64.zip",
            "bun-windows-x64-baseline-profile.zip",
            "bun-windows-x64-baseline.zip",
            "bun-windows-x64-profile.zip",
            "bun-windows-x64.zip",
            "SHASUMS256.txt",
            "SHASUMS256.txt.asc",
        ]
    }

    #[test]
    fn test_oven_sh_bun_bun_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 17),
                (Platform::LinuxAarch64, 9),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 23),
                (Platform::WinArm64, 19),
            ],
            &oven_sh_bun_bun_names(),
            "bun",
        );
    }

    fn owenlamont_ryl_ryl_names() -> Vec<&'static str> {
        vec![
            "ryl-aarch64-apple-darwin.tar.gz",
            "ryl-aarch64-pc-windows-msvc.zip",
            "ryl-aarch64-unknown-linux-gnu.tar.gz",
            "ryl-aarch64-unknown-linux-musl.tar.gz",
            "ryl-armv7-unknown-linux-gnueabihf.tar.gz",
            "ryl-i686-unknown-linux-gnu.tar.gz",
            "ryl-powerpc64le-unknown-linux-gnu.tar.gz",
            "ryl-s390x-unknown-linux-gnu.tar.gz",
            "ryl-x86_64-pc-windows-msvc.zip",
            "ryl-x86_64-unknown-linux-gnu.tar.gz",
            "ryl-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_owenlamont_ryl_ryl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 3),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 8),
                (Platform::WinArm64, 1),
            ],
            &owenlamont_ryl_ryl_names(),
            "ryl",
        );
    }

    fn pamburus_hl_hl_names() -> Vec<&'static str> {
        vec![
            "hl-linux-arm64-gnu.tar.gz",
            "hl-linux-arm64-musl.tar.gz",
            "hl-linux-x86_64-gnu.tar.gz",
            "hl-linux-x86_64-musl.tar.gz",
            "hl-macos-arm64.tar.gz",
            "hl-macos-x86_64.tar.gz",
            "hl-macos.tar.gz",
            "hl-windows-arm64.zip",
            "hl-windows.zip",
        ]
    }

    #[test]
    fn test_pamburus_hl_hl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 4),
                (Platform::Win32, 8),
                (Platform::Win64, 8),
                (Platform::WinArm64, 8),
            ],
            &pamburus_hl_hl_names(),
            "hl",
        );
    }

    fn particledecay_kconf_kconf_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "kconf-darwin-arm64-2.0.0.tar.gz",
            "kconf-darwin-x86_64-2.0.0.tar.gz",
            "kconf-linux-386-2.0.0.tar.gz",
            "kconf-linux-arm64-2.0.0.tar.gz",
            "kconf-linux-x86_64-2.0.0.tar.gz",
            "kconf-windows-386-2.0.0.tar.gz",
            "kconf-windows-arm64-2.0.0.tar.gz",
            "kconf-windows-x86_64-2.0.0.tar.gz",
        ]
    }

    #[test]
    fn test_particledecay_kconf_kconf_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 3),
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win32, 6),
                (Platform::Win64, 8),
                (Platform::WinArm64, 7),
            ],
            &particledecay_kconf_kconf_names(),
            "kconf",
        );
    }

    fn peak_s5cmd_s5cmd_names() -> Vec<&'static str> {
        vec![
            "s5cmd_2.3.0_Linux-32bit.tar.gz",
            "s5cmd_2.3.0_Linux-64bit.tar.gz",
            "s5cmd_2.3.0_Linux-arm64.tar.gz",
            "s5cmd_2.3.0_Linux-armv6.tar.gz",
            "s5cmd_2.3.0_Linux-ppc64le.tar.gz",
            "s5cmd_2.3.0_linux_386.deb",
            "s5cmd_2.3.0_linux_amd64.deb",
            "s5cmd_2.3.0_linux_arm64.deb",
            "s5cmd_2.3.0_linux_armv6.deb",
            "s5cmd_2.3.0_linux_ppc64le.deb",
            "s5cmd_2.3.0_macOS-64bit.tar.gz",
            "s5cmd_2.3.0_macOS-arm64.tar.gz",
            "s5cmd_2.3.0_Windows-32bit.zip",
            "s5cmd_2.3.0_Windows-64bit.zip",
            "s5cmd_2.3.0_Windows-arm64.zip",
            "s5cmd_2.3.0_Windows-armv6.zip",
            "s5cmd_checksums.txt",
        ]
    }

    #[test]
    fn test_peak_s5cmd_s5cmd_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 0),
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 11),
                (Platform::Win32, 12),
                (Platform::Win64, 13),
                (Platform::WinArm64, 14),
            ],
            &peak_s5cmd_s5cmd_names(),
            "s5cmd",
        );
    }

    fn pen_lang_pen_pen_names() -> Vec<&'static str> {
        vec![
            "pen-0.6.9-aarch64-apple-darwin.tar.xz",
            "pen-0.6.9-x86_64-unknown-linux-gnu.tar.xz",
        ]
    }

    #[test]
    fn test_pen_lang_pen_pen_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::OsxArm64, 0),
            ],
            &pen_lang_pen_pen_names(),
            "pen",
        );
    }

    fn peteretelej_tree_tree_names() -> Vec<&'static str> {
        vec![
            "tree-v1.3.0-Linux-amd64.tar.gz",
            "tree-v1.3.0-Linux-arm64.tar.gz",
            "tree-v1.3.0-macOS-amd64.tar.gz",
            "tree-v1.3.0-macOS-arm64.tar.gz",
            "tree-v1.3.0-Windows-32bit.msi",
            "tree-v1.3.0-Windows-32bit.zip",
            "tree-v1.3.0-Windows-64bit.msi",
            "tree-v1.3.0-Windows-64bit.zip",
            "tree-v1.3.0-Windows-arm64.msi",
            "tree-v1.3.0-Windows-arm64.zip",
            "tree-v1.3.0_Windows-32bit.exe",
            "tree-v1.3.0_Windows-64bit.exe",
            "tree-v1.3.0_Windows-arm64.exe",
        ]
    }

    #[test]
    fn test_peteretelej_tree_tree_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 7),
                (Platform::WinArm64, 9),
            ],
            &peteretelej_tree_tree_names(),
            "tree",
        );
    }

    fn pimalaya_himalaya_himalaya_names() -> Vec<&'static str> {
        vec![
            "himalaya.aarch64-darwin.tgz",
            "himalaya.aarch64-linux.tgz",
            "himalaya.armv6l-linux.tgz",
            "himalaya.armv7l-linux.tgz",
            "himalaya.i686-linux.tgz",
            "himalaya.x86_64-darwin.tgz",
            "himalaya.x86_64-linux.tgz",
            "himalaya.x86_64-windows.tgz",
            "himalaya.x86_64-windows.zip",
        ]
    }

    #[test]
    fn test_pimalaya_himalaya_himalaya_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 7),
            ],
            &pimalaya_himalaya_himalaya_names(),
            "himalaya",
        );
    }

    fn pkolaczk_fclones_fclones_names() -> Vec<&'static str> {
        vec![
            "fclones-0.35.0-2.x86_64.rpm",
            "fclones-0.35.0-linux-glibc-x86_64.tar.gz",
            "fclones-0.35.0-linux-musl-i686.tar.gz",
            "fclones-0.35.0-linux-musl-x86_64.tar.gz",
            "fclones-0.35.0-windows-x86_64.zip",
            "fclones_0.35.0-1_amd64.deb",
        ]
    }

    #[test]
    fn test_pkolaczk_fclones_fclones_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::Win64, 4),
            ],
            &pkolaczk_fclones_fclones_names(),
            "fclones",
        );
    }

    fn pluveto_upgit_upgit_names() -> Vec<&'static str> {
        vec![
            "upgit_cgo_linux_amd64",
            "upgit_linux_386",
            "upgit_linux_amd64",
            "upgit_linux_arm",
            "upgit_linux_arm64",
            "upgit_macos_amd64",
            "upgit_macos_arm64",
            "upgit_win_386.exe",
            "upgit_win_amd64.exe",
            "upgit_win_arm.exe",
            "upgit_win_arm64.exe",
        ]
    }

    #[test]
    fn test_pluveto_upgit_upgit_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 1),
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 6),
            ],
            &pluveto_upgit_upgit_names(),
            "upgit",
        );
    }

    fn praetorian_inc_gokart_gokart_names() -> Vec<&'static str> {
        vec![
            "gokart_0.5.1_checksums.txt",
            "gokart_0.5.1_darwin_macOS_x86_64.tar.gz",
            "gokart_0.5.1_linux_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_praetorian_inc_gokart_gokart_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 1),
            ],
            &praetorian_inc_gokart_gokart_names(),
            "gokart",
        );
    }

    fn printfn_fend_fend_names() -> Vec<&'static str> {
        vec![
            "fend-1.5.8-linux-aarch64-gnu.zip",
            "fend-1.5.8-linux-x86_64-gnu.zip",
            "fend-1.5.8-linux-x86_64-musl.zip",
            "fend-1.5.8-macos-aarch64.zip",
            "fend-1.5.8-windows-x64-exe.zip",
            "fend-1.5.8-windows-x64.msi",
            "fend.1",
        ]
    }

    #[test]
    fn test_printfn_fend_fend_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 0),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 4),
            ],
            &printfn_fend_fend_names(),
            "fend",
        );
    }

    fn projectdiscovery_httpx_httpx_names() -> Vec<&'static str> {
        vec![
            "httpx_1.8.1_checksums.txt",
            "httpx_1.8.1_linux_386.zip",
            "httpx_1.8.1_linux_amd64.zip",
            "httpx_1.8.1_linux_arm.zip",
            "httpx_1.8.1_linux_arm64.zip",
            "httpx_1.8.1_macOS_amd64.zip",
            "httpx_1.8.1_macOS_arm64.zip",
            "httpx_1.8.1_windows_386.zip",
            "httpx_1.8.1_windows_amd64.zip",
        ]
    }

    #[test]
    fn test_projectdiscovery_httpx_httpx_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 1),
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 6),
                (Platform::Win64, 8),
            ],
            &projectdiscovery_httpx_httpx_names(),
            "httpx",
        );
    }

    fn projectdiscovery_katana_katana_names() -> Vec<&'static str> {
        vec![
            "katana-linux-checksums.txt",
            "katana-mac-checksums.txt",
            "katana-windows-checksums.txt",
            "katana_1.4.0_linux_386.zip",
            "katana_1.4.0_linux_amd64.zip",
            "katana_1.4.0_linux_arm64.zip",
            "katana_1.4.0_macOS_amd64.zip",
            "katana_1.4.0_macOS_arm64.zip",
            "katana_1.4.0_windows_386.zip",
            "katana_1.4.0_windows_amd64.zip",
            "katana_1.4.0_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_projectdiscovery_katana_katana_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 3),
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 7),
                (Platform::Win32, 8),
                (Platform::Win64, 9),
                (Platform::WinArm64, 10),
            ],
            &projectdiscovery_katana_katana_names(),
            "katana",
        );
    }

    fn projectdiscovery_naabu_naabu_names() -> Vec<&'static str> {
        vec![
            "naabu-linux-amd64-checksums.txt",
            "naabu-linux-arm64-checksums.txt",
            "naabu-mac-checksums.txt",
            "naabu-windows-checksums.txt",
            "naabu_2.4.0_linux_amd64.zip",
            "naabu_2.4.0_linux_arm64.zip",
            "naabu_2.4.0_macOS_amd64.zip",
            "naabu_2.4.0_macOS_arm64.zip",
            "naabu_2.4.0_windows_386.zip",
            "naabu_2.4.0_windows_amd64.zip",
            "naabu_2.4.0_windows_arm64.zip",
        ]
    }

    #[test]
    fn test_projectdiscovery_naabu_naabu_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 7),
                (Platform::Win32, 8),
                (Platform::Win64, 9),
                (Platform::WinArm64, 10),
            ],
            &projectdiscovery_naabu_naabu_names(),
            "naabu",
        );
    }

    fn protocolbuffers_protobuf_protoc_names() -> Vec<&'static str> {
        vec![
            "protobuf-34.0.bazel.tar.gz",
            "protobuf-34.0.tar.gz",
            "protobuf-34.0.zip",
            "protoc-34.0-linux-aarch_64.zip",
            "protoc-34.0-linux-ppcle_64.zip",
            "protoc-34.0-linux-s390_64.zip",
            "protoc-34.0-linux-x86_32.zip",
            "protoc-34.0-linux-x86_64.zip",
            "protoc-34.0-osx-aarch_64.zip",
            "protoc-34.0-osx-universal_binary.zip",
            "protoc-34.0-osx-x86_64.zip",
            "protoc-34.0-win32.zip",
            "protoc-34.0-win64.zip",
        ]
    }

    #[test]
    fn test_protocolbuffers_protobuf_protoc_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 8),
                (Platform::Win64, 12),
            ],
            &protocolbuffers_protobuf_protoc_names(),
            "protoc",
        );
    }

    fn psastras_sarif_rs_hadolint_sarif_names() -> Vec<&'static str> {
        vec![
            "hadolint-sarif-aarch64-apple-darwin",
            "hadolint-sarif-x86_64-unknown-linux-gnu",
        ]
    }

    #[test]
    fn test_psastras_sarif_rs_hadolint_sarif_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::OsxArm64, 0),
            ],
            &psastras_sarif_rs_hadolint_sarif_names(),
            "hadolint-sarif",
        );
    }

    fn psf_black_black_names() -> Vec<&'static str> {
        vec![
            "black_linux",
            "black_linux-arm",
            "black_macos",
            "black_windows-arm.exe",
            "black_windows.exe",
        ]
    }

    #[test]
    fn test_psf_black_black_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 0),
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 2),
            ],
            &psf_black_black_names(),
            "black",
        );
    }

    fn pulumi_esc_esc_names() -> Vec<&'static str> {
        vec![
            "esc-0.22.0-checksums.txt",
            "esc-v0.22.0-darwin-arm64.tar.gz",
            "esc-v0.22.0-darwin-x64.tar.gz",
            "esc-v0.22.0-linux-arm64.tar.gz",
            "esc-v0.22.0-linux-x64.tar.gz",
            "esc-v0.22.0-windows-arm64.zip",
            "esc-v0.22.0-windows-x64.zip",
        ]
    }

    #[test]
    fn test_pulumi_esc_esc_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 6),
                (Platform::WinArm64, 5),
            ],
            &pulumi_esc_esc_names(),
            "esc",
        );
    }

    fn purpleclay_dns53_dns53_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "checksums.txt.pem",
            "checksums.txt.sig",
            "dns53-0.11.0-1.aarch64.rpm",
            "dns53-0.11.0-1.armv7hl.rpm",
            "dns53-0.11.0-1.i386.rpm",
            "dns53-0.11.0-1.x86_64.rpm",
            "dns53_0.11.0_aarch64.apk",
            "dns53_0.11.0_amd64.deb",
            "dns53_0.11.0_arm64.deb",
            "dns53_0.11.0_armhf.deb",
            "dns53_0.11.0_armv7.apk",
            "dns53_0.11.0_darwin-arm64.tar.gz",
            "dns53_0.11.0_darwin-arm64.tar.gz.sbom",
            "dns53_0.11.0_darwin-x86_64.tar.gz",
            "dns53_0.11.0_darwin-x86_64.tar.gz.sbom",
            "dns53_0.11.0_i386.deb",
            "dns53_0.11.0_linux-386.tar.gz",
            "dns53_0.11.0_linux-386.tar.gz.sbom",
            "dns53_0.11.0_linux-arm64.tar.gz",
            "dns53_0.11.0_linux-arm64.tar.gz.sbom",
            "dns53_0.11.0_linux-armv7.tar.gz",
            "dns53_0.11.0_linux-armv7.tar.gz.sbom",
            "dns53_0.11.0_linux-x86_64.tar.gz",
            "dns53_0.11.0_linux-x86_64.tar.gz.sbom",
            "dns53_0.11.0_windows-386.zip",
            "dns53_0.11.0_windows-386.zip.sbom",
            "dns53_0.11.0_windows-arm64.zip",
            "dns53_0.11.0_windows-arm64.zip.sbom",
            "dns53_0.11.0_windows-armv7.zip",
            "dns53_0.11.0_windows-armv7.zip.sbom",
            "dns53_0.11.0_windows-x86_64.zip",
            "dns53_0.11.0_windows-x86_64.zip.sbom",
            "dns53_0.11.0_x86.apk",
            "dns53_0.11.0_x86_64.apk",
        ]
    }

    #[test]
    fn test_purpleclay_dns53_dns53_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 17),
                (Platform::Linux64, 23),
                (Platform::LinuxAarch64, 19),
                (Platform::Osx64, 14),
                (Platform::OsxArm64, 12),
                (Platform::Win32, 25),
                (Platform::Win64, 31),
                (Platform::WinArm64, 27),
            ],
            &purpleclay_dns53_dns53_names(),
            "dns53",
        );
    }

    fn pvolok_mprocs_mprocs_names() -> Vec<&'static str> {
        vec![
            "mprocs-0.8.3-darwin-aarch64.tar.gz",
            "mprocs-0.8.3-darwin-x86_64.tar.gz",
            "mprocs-0.8.3-linux-aarch64-musl.tar.gz",
            "mprocs-0.8.3-linux-x86_64-musl.tar.gz",
            "mprocs-0.8.3-windows-x86_64.zip",
        ]
    }

    #[test]
    fn test_pvolok_mprocs_mprocs_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 4),
            ],
            &pvolok_mprocs_mprocs_names(),
            "mprocs",
        );
    }

    fn qarmin_czkawka_czkawka_names() -> Vec<&'static str> {
        vec![
            "linux_czkawka_cli_arm64",
            "linux_czkawka_cli_heif_raw_arm64",
            "linux_czkawka_cli_heif_raw_x86_64",
            "linux_czkawka_cli_musl",
            "linux_czkawka_cli_x86_64",
            "linux_czkawka_gui_arm64",
            "linux_czkawka_gui_heif_raw_arm64",
            "linux_czkawka_gui_heif_raw_x86_64",
            "linux_czkawka_gui_x86_64",
            "linux_krokiet_all_backends_arm64",
            "linux_krokiet_all_backends_x86_64",
            "linux_krokiet_arm64",
            "linux_krokiet_femtovg_wgpu_arm64",
            "linux_krokiet_femtovg_wgpu_x86_64",
            "linux_krokiet_heif_raw_all_backends_arm64",
            "linux_krokiet_heif_raw_all_backends_x86_64",
            "linux_krokiet_heif_raw_arm64",
            "linux_krokiet_heif_raw_femtovg_wgpu_arm64",
            "linux_krokiet_heif_raw_femtovg_wgpu_x86_64",
            "linux_krokiet_heif_raw_skia_opengl_arm64",
            "linux_krokiet_heif_raw_skia_opengl_x86_64",
            "linux_krokiet_heif_raw_skia_vulkan_arm64",
            "linux_krokiet_heif_raw_skia_vulkan_x86_64",
            "linux_krokiet_heif_raw_x86_64",
            "linux_krokiet_skia_opengl_arm64",
            "linux_krokiet_skia_opengl_x86_64",
            "linux_krokiet_skia_vulkan_arm64",
            "linux_krokiet_skia_vulkan_x86_64",
            "linux_krokiet_x86_64",
            "mac_czkawka_cli_arm64",
            "mac_czkawka_cli_heif_avif_arm64",
            "mac_czkawka_cli_heif_avif_x86_64",
            "mac_czkawka_cli_x86_64",
            "mac_czkawka_gui_arm64",
            "mac_czkawka_gui_heif_avif_arm64",
            "mac_czkawka_gui_heif_avif_x86_64",
            "mac_czkawka_gui_x86_64",
            "mac_krokiet_all_backends_arm64",
            "mac_krokiet_all_backends_x86_64",
            "mac_krokiet_arm64",
            "mac_krokiet_femtovg_wgpu_arm64",
            "mac_krokiet_femtovg_wgpu_x86_64",
            "mac_krokiet_heif_avif_all_backends_arm64",
            "mac_krokiet_heif_avif_all_backends_x86_64",
            "mac_krokiet_heif_avif_arm64",
            "mac_krokiet_heif_avif_femtovg_wgpu_arm64",
            "mac_krokiet_heif_avif_femtovg_wgpu_x86_64",
            "mac_krokiet_heif_avif_x86_64",
            "mac_krokiet_skia_vulkan_arm64",
            "mac_krokiet_skia_vulkan_heif_avif_arm64",
            "mac_krokiet_skia_vulkan_heif_avif_x86_64",
            "mac_krokiet_skia_vulkan_x86_64",
            "mac_krokiet_x86_64",
            "windows_czkawka_cli.exe",
            "windows_czkawka_gui_gtk_412.zip",
            "windows_krokiet_on_linux.exe",
            "windows_krokiet_on_windows_all_backends.exe",
            "windows_krokiet_on_windows_femtovg_wgpu.exe",
            "windows_krokiet_on_windows_skia_opengl.exe",
            "windows_krokiet_on_windows_skia_vulkan.exe",
        ]
    }

    #[test]
    fn test_qarmin_czkawka_czkawka_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 32),
                (Platform::OsxArm64, 29),
            ],
            &qarmin_czkawka_czkawka_names(),
            "czkawka",
        );
    }

    fn quarto_dev_quarto_cli_quarto_names() -> Vec<&'static str> {
        vec![
            "changelog.md",
            "quarto-1.8.27-checksums.txt",
            "quarto-1.8.27-linux-aarch64.rpm",
            "quarto-1.8.27-linux-amd64.deb",
            "quarto-1.8.27-linux-amd64.tar.gz",
            "quarto-1.8.27-linux-arm64.deb",
            "quarto-1.8.27-linux-arm64.tar.gz",
            "quarto-1.8.27-linux-x86_64.rpm",
            "quarto-1.8.27-macos.pkg",
            "quarto-1.8.27-macos.tar.gz",
            "quarto-1.8.27-win.msi",
            "quarto-1.8.27-win.zip",
            "quarto-1.8.27.tar.gz",
        ]
    }

    #[test]
    fn test_quarto_dev_quarto_cli_quarto_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 9),
                (Platform::Win32, 11),
                (Platform::Win64, 11),
                (Platform::WinArm64, 11),
            ],
            &quarto_dev_quarto_cli_quarto_names(),
            "quarto",
        );
    }

    fn quarylabs_sqruff_sqruff_names() -> Vec<&'static str> {
        vec![
            "sqruff-0.34.1.vsix",
            "sqruff-darwin-aarch64.tar.gz",
            "sqruff-darwin-aarch64.tar.gz.sha256",
            "sqruff-darwin-x86_64.tar.gz",
            "sqruff-darwin-x86_64.tar.gz.sha256",
            "sqruff-linux-aarch64-musl.tar.gz",
            "sqruff-linux-aarch64-musl.tar.gz.sha256",
            "sqruff-linux-x86_64-musl.tar.gz",
            "sqruff-linux-x86_64-musl.tar.gz.sha256",
            "sqruff-windows-x86_64.zip",
            "sqruff-windows-x86_64.zip.sha256",
        ]
    }

    #[test]
    fn test_quarylabs_sqruff_sqruff_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 9),
            ],
            &quarylabs_sqruff_sqruff_names(),
            "sqruff",
        );
    }

    fn raskell_io_hx_hx_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "hx-v0.6.0-aarch64-apple-darwin.tar.gz",
            "hx-v0.6.0-aarch64-apple-darwin.tar.gz.sha256",
            "hx-v0.6.0-aarch64-unknown-linux-gnu.tar.gz",
            "hx-v0.6.0-aarch64-unknown-linux-gnu.tar.gz.sha256",
            "hx-v0.6.0-x86_64-apple-darwin.tar.gz",
            "hx-v0.6.0-x86_64-apple-darwin.tar.gz.sha256",
            "hx-v0.6.0-x86_64-pc-windows-msvc.zip",
            "hx-v0.6.0-x86_64-pc-windows-msvc.zip.sha256",
            "hx-v0.6.0-x86_64-unknown-linux-gnu.tar.gz",
            "hx-v0.6.0-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "hx-v0.6.0-x86_64-unknown-linux-musl.tar.gz",
            "hx-v0.6.0-x86_64-unknown-linux-musl.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_raskell_io_hx_hx_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 11),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 7),
            ],
            &raskell_io_hx_hx_names(),
            "hx",
        );
    }

    fn rclone_rclone_rclone_names() -> Vec<&'static str> {
        vec![
            "MD5SUMS",
            "rclone-v1.73.1-aix-ppc64.zip",
            "rclone-v1.73.1-freebsd-386.zip",
            "rclone-v1.73.1-freebsd-amd64.zip",
            "rclone-v1.73.1-freebsd-arm-v6.zip",
            "rclone-v1.73.1-freebsd-arm-v7.zip",
            "rclone-v1.73.1-freebsd-arm.zip",
            "rclone-v1.73.1-linux-386.deb",
            "rclone-v1.73.1-linux-386.rpm",
            "rclone-v1.73.1-linux-386.zip",
            "rclone-v1.73.1-linux-amd64.deb",
            "rclone-v1.73.1-linux-amd64.rpm",
            "rclone-v1.73.1-linux-amd64.zip",
            "rclone-v1.73.1-linux-arm-v6.deb",
            "rclone-v1.73.1-linux-arm-v6.rpm",
            "rclone-v1.73.1-linux-arm-v6.zip",
            "rclone-v1.73.1-linux-arm-v7.deb",
            "rclone-v1.73.1-linux-arm-v7.rpm",
            "rclone-v1.73.1-linux-arm-v7.zip",
            "rclone-v1.73.1-linux-arm.deb",
            "rclone-v1.73.1-linux-arm.rpm",
            "rclone-v1.73.1-linux-arm.zip",
            "rclone-v1.73.1-linux-arm64.deb",
            "rclone-v1.73.1-linux-arm64.rpm",
            "rclone-v1.73.1-linux-arm64.zip",
            "rclone-v1.73.1-linux-mips.deb",
            "rclone-v1.73.1-linux-mips.rpm",
            "rclone-v1.73.1-linux-mips.zip",
            "rclone-v1.73.1-linux-mipsle.deb",
            "rclone-v1.73.1-linux-mipsle.rpm",
            "rclone-v1.73.1-linux-mipsle.zip",
            "rclone-v1.73.1-netbsd-386.zip",
            "rclone-v1.73.1-netbsd-amd64.zip",
            "rclone-v1.73.1-netbsd-arm-v6.zip",
            "rclone-v1.73.1-netbsd-arm-v7.zip",
            "rclone-v1.73.1-netbsd-arm.zip",
            "rclone-v1.73.1-openbsd-386.zip",
            "rclone-v1.73.1-openbsd-amd64.zip",
            "rclone-v1.73.1-osx-amd64.zip",
            "rclone-v1.73.1-osx-arm64.zip",
            "rclone-v1.73.1-plan9-386.zip",
            "rclone-v1.73.1-plan9-amd64.zip",
            "rclone-v1.73.1-solaris-amd64.zip",
            "rclone-v1.73.1-vendor.tar.gz",
            "rclone-v1.73.1-windows-386.zip",
            "rclone-v1.73.1-windows-amd64.zip",
            "rclone-v1.73.1-windows-arm64.zip",
            "rclone-v1.73.1.tar.gz",
            "SHA1SUMS",
            "SHA256SUMS",
            "version.txt",
        ]
    }

    #[test]
    fn test_rclone_rclone_rclone_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 9),
                (Platform::Linux64, 12),
                (Platform::LinuxAarch64, 24),
                (Platform::Osx64, 38),
                (Platform::OsxArm64, 39),
                (Platform::Win32, 44),
                (Platform::Win64, 45),
                (Platform::WinArm64, 46),
            ],
            &rclone_rclone_rclone_names(),
            "rclone",
        );
    }

    fn rcoh_angle_grinder_agrind_names() -> Vec<&'static str> {
        vec![
            "agrind-x86_64-apple-darwin.tar.gz",
            "agrind-x86_64-unknown-linux-gnu.tar.gz",
            "agrind-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_rcoh_angle_grinder_agrind_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 0),
            ],
            &rcoh_angle_grinder_agrind_names(),
            "agrind",
        );
    }

    fn release_plz_release_plz_release_plz_names() -> Vec<&'static str> {
        vec![
            "release-plz-aarch64-apple-darwin.tar.gz",
            "release-plz-aarch64-pc-windows-msvc.tar.gz",
            "release-plz-aarch64-pc-windows-msvc.zip",
            "release-plz-aarch64-unknown-linux-gnu.tar.gz",
            "release-plz-aarch64-unknown-linux-musl.tar.gz",
            "release-plz-x86_64-pc-windows-msvc.tar.gz",
            "release-plz-x86_64-pc-windows-msvc.zip",
            "release-plz-x86_64-unknown-freebsd.tar.gz",
            "release-plz-x86_64-unknown-linux-gnu.tar.gz",
            "release-plz-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_release_plz_release_plz_release_plz_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 4),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 5),
                (Platform::WinArm64, 1),
            ],
            &release_plz_release_plz_release_plz_names(),
            "release-plz",
        );
    }

    fn rest_sh_restish_restish_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "restish-0.21.2-darwin-amd64.tar.gz",
            "restish-0.21.2-darwin-arm64.tar.gz",
            "restish-0.21.2-linux-amd64.tar.gz",
            "restish-0.21.2-linux-arm64.tar.gz",
            "restish-0.21.2-windows-amd64.zip",
        ]
    }

    #[test]
    fn test_rest_sh_restish_restish_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 5),
            ],
            &rest_sh_restish_restish_names(),
            "restish",
        );
    }

    fn rgwood_systemctl_tui_systemctl_tui_names() -> Vec<&'static str> {
        vec![
            "systemctl-tui-aarch64-unknown-linux-musl.tar.gz",
            "systemctl-tui-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_rgwood_systemctl_tui_systemctl_tui_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 0),
            ],
            &rgwood_systemctl_tui_systemctl_tui_names(),
            "systemctl-tui",
        );
    }

    fn rhysd_hgrep_hgrep_names() -> Vec<&'static str> {
        vec![
            "hgrep-v0.3.9-aarch64-apple-darwin.zip",
            "hgrep-v0.3.9-aarch64-pc-windows-msvc.zip",
            "hgrep-v0.3.9-aarch64-unknown-linux-gnu.zip",
            "hgrep-v0.3.9-x86_64-apple-darwin.zip",
            "hgrep-v0.3.9-x86_64-pc-windows-msvc.zip",
            "hgrep-v0.3.9-x86_64-unknown-linux-gnu.zip",
            "hgrep-v0.3.9-x86_64-unknown-linux-musl.zip",
            "hgrep_0.3.9-1_amd64.deb",
        ]
    }

    #[test]
    fn test_rhysd_hgrep_hgrep_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 4),
            ],
            &rhysd_hgrep_hgrep_names(),
            "hgrep",
        );
    }

    fn rootless_containers_rootlesskit_rootlesskit_names() -> Vec<&'static str> {
        vec![
            "rootlesskit-aarch64.tar.gz",
            "rootlesskit-armv7l.tar.gz",
            "rootlesskit-ppc64le.tar.gz",
            "rootlesskit-riscv64.tar.gz",
            "rootlesskit-s390x.tar.gz",
            "rootlesskit-x86_64.tar.gz",
            "SHA256SUMS",
            "SHA256SUMS.asc",
        ]
    }

    #[test]
    fn test_rootless_containers_rootlesskit_rootlesskit_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 0),
            ],
            &rootless_containers_rootlesskit_rootlesskit_names(),
            "rootlesskit",
        );
    }

    fn rossmacarthur_sheldon_sheldon_names() -> Vec<&'static str> {
        vec![
            "sheldon-0.8.5-aarch64-apple-darwin.tar.gz",
            "sheldon-0.8.5-aarch64-unknown-linux-musl.tar.gz",
            "sheldon-0.8.5-armv7-unknown-linux-musleabihf.tar.gz",
            "sheldon-0.8.5-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_rossmacarthur_sheldon_sheldon_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 1),
                (Platform::OsxArm64, 0),
            ],
            &rossmacarthur_sheldon_sheldon_names(),
            "sheldon",
        );
    }

    fn rui314_mold_mold_names() -> Vec<&'static str> {
        vec![
            "mold-2.40.4-aarch64-linux.tar.gz",
            "mold-2.40.4-arm-linux.tar.gz",
            "mold-2.40.4-loongarch64-linux.tar.gz",
            "mold-2.40.4-ppc64le-linux.tar.gz",
            "mold-2.40.4-riscv64-linux.tar.gz",
            "mold-2.40.4-s390x-linux.tar.gz",
            "mold-2.40.4-x86_64-linux.tar.gz",
            "mold-2.40.4-x86_64-windows.zip",
        ]
    }

    #[test]
    fn test_rui314_mold_mold_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 0),
            ],
            &rui314_mold_mold_names(),
            "mold",
        );
    }

    fn rust_cross_cargo_zigbuild_cargo_zigbuild_names() -> Vec<&'static str> {
        vec![
            "cargo-zigbuild-aarch64-apple-darwin.tar.xz",
            "cargo-zigbuild-aarch64-apple-darwin.tar.xz.sha256",
            "cargo-zigbuild-aarch64-pc-windows-msvc.zip",
            "cargo-zigbuild-aarch64-pc-windows-msvc.zip.sha256",
            "cargo-zigbuild-aarch64-unknown-linux-gnu.tar.xz",
            "cargo-zigbuild-aarch64-unknown-linux-gnu.tar.xz.sha256",
            "cargo-zigbuild-installer.ps1",
            "cargo-zigbuild-installer.sh",
            "cargo-zigbuild-x86_64-apple-darwin.tar.xz",
            "cargo-zigbuild-x86_64-apple-darwin.tar.xz.sha256",
            "cargo-zigbuild-x86_64-pc-windows-msvc.zip",
            "cargo-zigbuild-x86_64-pc-windows-msvc.zip.sha256",
            "cargo-zigbuild-x86_64-unknown-linux-gnu.tar.xz",
            "cargo-zigbuild-x86_64-unknown-linux-gnu.tar.xz.sha256",
            "cargo-zigbuild-x86_64-unknown-linux-musl.tar.xz",
            "cargo-zigbuild-x86_64-unknown-linux-musl.tar.xz.sha256",
            "dist-manifest.json",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_rust_cross_cargo_zigbuild_cargo_zigbuild_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 12),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 8),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 10),
                (Platform::WinArm64, 2),
            ],
            &rust_cross_cargo_zigbuild_cargo_zigbuild_names(),
            "cargo-zigbuild",
        );
    }

    fn rust_lang_rust_analyzer_rust_analyzer_names() -> Vec<&'static str> {
        vec![
            "rust-analyzer-aarch64-apple-darwin.gz",
            "rust-analyzer-aarch64-pc-windows-msvc.zip",
            "rust-analyzer-aarch64-unknown-linux-gnu.gz",
            "rust-analyzer-alpine-x64.vsix",
            "rust-analyzer-arm-unknown-linux-gnueabihf.gz",
            "rust-analyzer-darwin-arm64.vsix",
            "rust-analyzer-darwin-x64.vsix",
            "rust-analyzer-i686-pc-windows-msvc.zip",
            "rust-analyzer-linux-arm64.vsix",
            "rust-analyzer-linux-armhf.vsix",
            "rust-analyzer-linux-x64.vsix",
            "rust-analyzer-no-server.vsix",
            "rust-analyzer-win32-arm64.vsix",
            "rust-analyzer-win32-x64.vsix",
            "rust-analyzer-x86_64-apple-darwin.gz",
            "rust-analyzer-x86_64-pc-windows-msvc.zip",
            "rust-analyzer-x86_64-unknown-linux-gnu.gz",
            "rust-analyzer-x86_64-unknown-linux-musl.gz",
        ]
    }

    #[test]
    fn test_rust_lang_rust_analyzer_rust_analyzer_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 16),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 14),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 15),
                (Platform::WinArm64, 1),
            ],
            &rust_lang_rust_analyzer_rust_analyzer_names(),
            "rust-analyzer",
        );
    }

    fn ryoppippi_zigchat_zigchat_names() -> Vec<&'static str> {
        vec![
            "zigchat-aarch64-linux.tar.gz",
            "zigchat-aarch64-macos.tar.gz",
            "zigchat-aarch64-windows.zip",
            "zigchat-x86_64-linux.tar.gz",
            "zigchat-x86_64-macos.tar.gz",
            "zigchat-x86_64-windows.zip",
        ]
    }

    #[test]
    fn test_ryoppippi_zigchat_zigchat_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 5),
                (Platform::WinArm64, 2),
            ],
            &ryoppippi_zigchat_zigchat_names(),
            "zigchat",
        );
    }

    fn s0md3v_smap_smap_names() -> Vec<&'static str> {
        vec![
            "smap_0.1.12--sha256_checksums.txt",
            "smap_0.1.12_freebsd_amd64.tar.xz",
            "smap_0.1.12_linux_amd64.tar.xz",
            "smap_0.1.12_linux_arm64.tar.xz",
            "smap_0.1.12_linux_arm7.tar.xz",
            "smap_0.1.12_macOS_amd64.tar.xz",
            "smap_0.1.12_macOS_arm64.tar.xz",
            "smap_0.1.12_windows_amd64.zip",
        ]
    }

    #[test]
    fn test_s0md3v_smap_smap_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 6),
                (Platform::Win64, 7),
            ],
            &s0md3v_smap_smap_names(),
            "smap",
        );
    }

    fn sachaos_viddy_viddy_names() -> Vec<&'static str> {
        vec![
            "viddy-v1.3.0-linux-arm64.sha256",
            "viddy-v1.3.0-linux-arm64.tar.gz",
            "viddy-v1.3.0-linux-i686.sha256",
            "viddy-v1.3.0-linux-i686.tar.gz",
            "viddy-v1.3.0-linux-x86_64.sha256",
            "viddy-v1.3.0-linux-x86_64.tar.gz",
            "viddy-v1.3.0-macos-arm64.sha256",
            "viddy-v1.3.0-macos-arm64.tar.gz",
            "viddy-v1.3.0-macos-x86_64.sha256",
            "viddy-v1.3.0-macos-x86_64.tar.gz",
            "viddy-v1.3.0-windows-x86_64.sha256",
            "viddy-v1.3.0-windows-x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_sachaos_viddy_viddy_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 7),
                (Platform::Win64, 11),
            ],
            &sachaos_viddy_viddy_names(),
            "viddy",
        );
    }

    fn samuel_lucas6_kryptor_kryptor_names() -> Vec<&'static str> {
        vec![
            "kryptor-linux-arm64.zip",
            "kryptor-linux-arm64.zip.digest",
            "kryptor-linux-arm64.zip.signature",
            "kryptor-linux-x64.zip",
            "kryptor-linux-x64.zip.digest",
            "kryptor-linux-x64.zip.signature",
            "kryptor-macos-arm64.zip",
            "kryptor-macos-arm64.zip.digest",
            "kryptor-macos-arm64.zip.signature",
            "kryptor-macos-x64.zip",
            "kryptor-macos-x64.zip.digest",
            "kryptor-macos-x64.zip.signature",
            "kryptor-windows-x64.zip",
            "kryptor-windows-x64.zip.digest",
            "kryptor-windows-x64.zip.signature",
        ]
    }

    #[test]
    fn test_samuel_lucas6_kryptor_kryptor_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 6),
                (Platform::Win64, 12),
            ],
            &samuel_lucas6_kryptor_kryptor_names(),
            "kryptor",
        );
    }

    fn sanathp_statusok_statusok_names() -> Vec<&'static str> {
        vec![
            "statusok_linux.zip",
            "statusok_mac.zip",
        ]
    }

    #[test]
    fn test_sanathp_statusok_statusok_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
            ],
            &sanathp_statusok_statusok_names(),
            "statusok",
        );
    }

    fn sarub0b0_kubetui_kubetui_names() -> Vec<&'static str> {
        vec![
            "kubetui-aarch64-apple-darwin",
            "kubetui-x86_64-apple-darwin",
            "kubetui-x86_64-pc-windows-msvc.exe",
            "kubetui-x86_64-unknown-linux-gnu",
            "kubetui-x86_64-unknown-linux-musl",
        ]
    }

    #[test]
    fn test_sarub0b0_kubetui_kubetui_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
            ],
            &sarub0b0_kubetui_kubetui_names(),
            "kubetui",
        );
    }

    fn sass_dart_sass_dart_sass_names() -> Vec<&'static str> {
        vec![
            "dart-sass-1.97.3-android-arm.tar.gz",
            "dart-sass-1.97.3-android-arm64.tar.gz",
            "dart-sass-1.97.3-android-riscv64.tar.gz",
            "dart-sass-1.97.3-android-x64.tar.gz",
            "dart-sass-1.97.3-linux-arm-musl.tar.gz",
            "dart-sass-1.97.3-linux-arm.tar.gz",
            "dart-sass-1.97.3-linux-arm64-musl.tar.gz",
            "dart-sass-1.97.3-linux-arm64.tar.gz",
            "dart-sass-1.97.3-linux-riscv64-musl.tar.gz",
            "dart-sass-1.97.3-linux-riscv64.tar.gz",
            "dart-sass-1.97.3-linux-x64-musl.tar.gz",
            "dart-sass-1.97.3-linux-x64.tar.gz",
            "dart-sass-1.97.3-macos-arm64.tar.gz",
            "dart-sass-1.97.3-macos-x64.tar.gz",
            "dart-sass-1.97.3-windows-arm64.zip",
            "dart-sass-1.97.3-windows-x64.zip",
        ]
    }

    #[test]
    fn test_sass_dart_sass_dart_sass_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 10),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 13),
                (Platform::OsxArm64, 12),
                (Platform::Win64, 15),
                (Platform::WinArm64, 14),
            ],
            &sass_dart_sass_dart_sass_names(),
            "dart-sass",
        );
    }

    fn satococoa_wtp_wtp_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "wtp_2.10.0_Darwin_arm64.tar.gz",
            "wtp_2.10.0_linux_arm64.apk",
            "wtp_2.10.0_linux_arm64.deb",
            "wtp_2.10.0_linux_arm64.rpm",
            "wtp_2.10.0_Linux_arm64.tar.gz",
            "wtp_2.10.0_linux_x86_64.apk",
            "wtp_2.10.0_linux_x86_64.deb",
            "wtp_2.10.0_linux_x86_64.rpm",
            "wtp_2.10.0_Linux_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_satococoa_wtp_wtp_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 5),
                (Platform::OsxArm64, 1),
            ],
            &satococoa_wtp_wtp_names(),
            "wtp",
        );
    }

    fn saucelabs_forwarder_forwarder_names() -> Vec<&'static str> {
        vec![
            "checksums",
            "forwarder-1.6.0_darwin.all.zip",
            "forwarder-1.6.0_linux.aarch64.rpm",
            "forwarder-1.6.0_linux.aarch64.tar.gz",
            "forwarder-1.6.0_linux.x86_64.rpm",
            "forwarder-1.6.0_linux.x86_64.tar.gz",
            "forwarder-1.6.0_windows.aarch64.zip",
            "forwarder-1.6.0_windows.x86_64.zip",
            "forwarder_1.6.0.linux_amd64.deb",
            "forwarder_1.6.0.linux_arm64.deb",
        ]
    }

    #[test]
    fn test_saucelabs_forwarder_forwarder_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 7),
                (Platform::WinArm64, 6),
            ],
            &saucelabs_forwarder_forwarder_names(),
            "forwarder",
        );
    }

    fn schollz_croc_croc_names() -> Vec<&'static str> {
        vec![
            "croc_v10.3.1_checksums.txt",
            "croc_v10.3.1_DragonFlyBSD-64bit.tar.gz",
            "croc_v10.3.1_FreeBSD-64bit.tar.gz",
            "croc_v10.3.1_FreeBSD-ARM64.tar.gz",
            "croc_v10.3.1_Linux-32bit.tar.gz",
            "croc_v10.3.1_Linux-64bit.tar.gz",
            "croc_v10.3.1_Linux-ARM.tar.gz",
            "croc_v10.3.1_Linux-ARM64.tar.gz",
            "croc_v10.3.1_Linux-ARMv5.tar.gz",
            "croc_v10.3.1_Linux-RISCV64.tar.gz",
            "croc_v10.3.1_macOS-64bit.tar.gz",
            "croc_v10.3.1_macOS-ARM64.tar.gz",
            "croc_v10.3.1_NetBSD-32bit.tar.gz",
            "croc_v10.3.1_NetBSD-64bit.tar.gz",
            "croc_v10.3.1_NetBSD-ARM64.tar.gz",
            "croc_v10.3.1_OpenBSD-64bit.tar.gz",
            "croc_v10.3.1_OpenBSD-ARM64.tar.gz",
            "croc_v10.3.1_src.tar.gz",
            "croc_v10.3.1_Windows-32bit.zip",
            "croc_v10.3.1_Windows-64bit.zip",
            "croc_v10.3.1_Windows-ARM.zip",
            "croc_v10.3.1_Windows-ARM64.zip",
        ]
    }

    #[test]
    fn test_schollz_croc_croc_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 7),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 11),
                (Platform::Win64, 19),
                (Platform::WinArm64, 21),
            ],
            &schollz_croc_croc_names(),
            "croc",
        );
    }

    fn secretlint_secretlint_secretlint_names() -> Vec<&'static str> {
        vec![
            "secretlint-11.3.1-darwin-arm64",
            "secretlint-11.3.1-darwin-x64",
            "secretlint-11.3.1-linux-arm64",
            "secretlint-11.3.1-linux-x64",
            "secretlint-11.3.1-sha256sum.txt",
            "secretlint-11.3.1-windows-x64.exe",
        ]
    }

    #[test]
    fn test_secretlint_secretlint_secretlint_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
            ],
            &secretlint_secretlint_secretlint_names(),
            "secretlint",
        );
    }

    fn sharkdp_hyperfine_hyperfine_names() -> Vec<&'static str> {
        vec![
            "hyperfine-musl_1.20.0_amd64.deb",
            "hyperfine-musl_1.20.0_armhf.deb",
            "hyperfine-musl_1.20.0_i686.deb",
            "hyperfine-v1.20.0-aarch64-apple-darwin.tar.gz",
            "hyperfine-v1.20.0-aarch64-unknown-linux-gnu.tar.gz",
            "hyperfine-v1.20.0-arm-unknown-linux-gnueabihf.tar.gz",
            "hyperfine-v1.20.0-arm-unknown-linux-musleabihf.tar.gz",
            "hyperfine-v1.20.0-i686-pc-windows-msvc.zip",
            "hyperfine-v1.20.0-i686-unknown-linux-gnu.tar.gz",
            "hyperfine-v1.20.0-i686-unknown-linux-musl.tar.gz",
            "hyperfine-v1.20.0-x86_64-apple-darwin.tar.gz",
            "hyperfine-v1.20.0-x86_64-pc-windows-msvc.zip",
            "hyperfine-v1.20.0-x86_64-unknown-linux-gnu.tar.gz",
            "hyperfine-v1.20.0-x86_64-unknown-linux-musl.tar.gz",
            "hyperfine_1.20.0_amd64.deb",
            "hyperfine_1.20.0_arm64.deb",
            "hyperfine_1.20.0_armhf.deb",
            "hyperfine_1.20.0_i686.deb",
        ]
    }

    #[test]
    fn test_sharkdp_hyperfine_hyperfine_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 9),
                (Platform::Linux64, 13),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 11),
            ],
            &sharkdp_hyperfine_hyperfine_names(),
            "hyperfine",
        );
    }

    fn sharkdp_numbat_numbat_names() -> Vec<&'static str> {
        vec![
            "numbat-musl_1.23.0_amd64.deb",
            "numbat-musl_1.23.0_arm64.deb",
            "numbat-musl_1.23.0_armhf.deb",
            "numbat-musl_1.23.0_i686.deb",
            "numbat-v1.23.0-aarch64-apple-darwin.tar.gz",
            "numbat-v1.23.0-aarch64-unknown-linux-gnu.tar.gz",
            "numbat-v1.23.0-aarch64-unknown-linux-musl.tar.gz",
            "numbat-v1.23.0-arm-unknown-linux-gnueabihf.tar.gz",
            "numbat-v1.23.0-arm-unknown-linux-musleabihf.tar.gz",
            "numbat-v1.23.0-i686-pc-windows-msvc.zip",
            "numbat-v1.23.0-i686-unknown-linux-gnu.tar.gz",
            "numbat-v1.23.0-i686-unknown-linux-musl.tar.gz",
            "numbat-v1.23.0-x86_64-apple-darwin.tar.gz",
            "numbat-v1.23.0-x86_64-pc-windows-msvc.zip",
            "numbat-v1.23.0-x86_64-unknown-linux-gnu.tar.gz",
            "numbat-v1.23.0-x86_64-unknown-linux-musl.tar.gz",
            "numbat-vscode-extension-v0.1.1.vsix",
            "numbat_1.23.0_amd64.deb",
            "numbat_1.23.0_arm64.deb",
            "numbat_1.23.0_armhf.deb",
            "numbat_1.23.0_i686.deb",
        ]
    }

    #[test]
    fn test_sharkdp_numbat_numbat_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 15),
                (Platform::LinuxAarch64, 8),
                (Platform::Osx64, 12),
                (Platform::OsxArm64, 4),
                (Platform::Win64, 13),
            ],
            &sharkdp_numbat_numbat_names(),
            "numbat",
        );
    }

    fn sheepla_fzwiki_fzwiki_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "fzwiki_0.1.0-alpha_Darwin_arm64.tar.gz",
            "fzwiki_0.1.0-alpha_Darwin_x86_64.tar.gz",
            "fzwiki_0.1.0-alpha_Linux_arm64.tar.gz",
            "fzwiki_0.1.0-alpha_Linux_i386.tar.gz",
            "fzwiki_0.1.0-alpha_Linux_x86_64.tar.gz",
            "fzwiki_0.1.0-alpha_Windows_arm64.zip",
            "fzwiki_0.1.0-alpha_Windows_i386.zip",
            "fzwiki_0.1.0-alpha_Windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_sheepla_fzwiki_fzwiki_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 8),
                (Platform::WinArm64, 6),
            ],
            &sheepla_fzwiki_fzwiki_names(),
            "fzwiki",
        );
    }

    fn sigi_cli_sigi_sigi_names() -> Vec<&'static str> {
        vec![
            "sigi_v3.7.1_aarch64-apple-darwin.tar.gz",
            "sigi_v3.7.1_aarch64-apple-darwin.tar.gz.sha256sum",
            "sigi_v3.7.1_aarch64-apple-darwin.zip",
            "sigi_v3.7.1_aarch64-apple-darwin.zip.sha256sum",
            "sigi_v3.7.1_x86_64-apple-darwin.tar.gz",
            "sigi_v3.7.1_x86_64-apple-darwin.tar.gz.sha256sum",
            "sigi_v3.7.1_x86_64-apple-darwin.zip",
            "sigi_v3.7.1_x86_64-apple-darwin.zip.sha256sum",
            "sigi_v3.7.1_x86_64-pc-windows-gnu.tar.gz",
            "sigi_v3.7.1_x86_64-pc-windows-gnu.tar.gz.sha256sum",
            "sigi_v3.7.1_x86_64-pc-windows-gnu.zip",
            "sigi_v3.7.1_x86_64-pc-windows-gnu.zip.sha256sum",
            "sigi_v3.7.1_x86_64-unknown-linux-musl.tar.gz",
            "sigi_v3.7.1_x86_64-unknown-linux-musl.tar.gz.sha256sum",
            "sigi_v3.7.1_x86_64-unknown-linux-musl.zip",
            "sigi_v3.7.1_x86_64-unknown-linux-musl.zip.sha256sum",
        ]
    }

    #[test]
    fn test_sigi_cli_sigi_sigi_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 12),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 8),
            ],
            &sigi_cli_sigi_sigi_names(),
            "sigi",
        );
    }

    fn sirwart_ripsecrets_ripsecrets_names() -> Vec<&'static str> {
        vec![
            "ripsecrets-0.1.11-aarch64-apple-darwin.tar.gz",
            "ripsecrets-0.1.11-x86_64-apple-darwin.tar.gz",
            "ripsecrets-0.1.11-x86_64-unknown-linux-gnu.tar.gz",
        ]
    }

    #[test]
    fn test_sirwart_ripsecrets_ripsecrets_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
            ],
            &sirwart_ripsecrets_ripsecrets_names(),
            "ripsecrets",
        );
    }

    fn skaji_relocatable_perl_perl_names() -> Vec<&'static str> {
        vec![
            "perl-darwin-amd64.tar.gz",
            "perl-darwin-amd64.tar.xz",
            "perl-darwin-arm64.tar.gz",
            "perl-darwin-arm64.tar.xz",
            "perl-linux-amd64.tar.gz",
            "perl-linux-amd64.tar.xz",
            "perl-linux-arm64.tar.gz",
            "perl-linux-arm64.tar.xz",
        ]
    }

    #[test]
    fn test_skaji_relocatable_perl_perl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 2),
            ],
            &skaji_relocatable_perl_perl_names(),
            "perl",
        );
    }

    fn skanehira_ghost_ghost_names() -> Vec<&'static str> {
        vec![
            "ghost_aarch64-apple-darwin.tar.gz",
            "ghost_aarch64-unknown-linux-musl.tar.gz",
            "ghost_x86_64-apple-darwin.tar.gz",
            "ghost_x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_skanehira_ghost_ghost_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 0),
            ],
            &skanehira_ghost_ghost_names(),
            "ghost",
        );
    }

    fn skanehira_gjo_gjo_names() -> Vec<&'static str> {
        vec![
            "Linux.zip",
            "MacOS.zip",
            "Windows.zip",
        ]
    }

    #[test]
    fn test_skanehira_gjo_gjo_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
                (Platform::Win32, 2),
                (Platform::Win64, 2),
                (Platform::WinArm64, 2),
            ],
            &skanehira_gjo_gjo_names(),
            "gjo",
        );
    }

    fn skeema_skeema_skeema_names() -> Vec<&'static str> {
        vec![
            "skeema_1.13.2_linux_amd64.tar.gz",
            "skeema_1.13.2_linux_arm64.tar.gz",
            "skeema_1.13.2_mac_amd64.tar.gz",
            "skeema_1.13.2_mac_amd64.zip",
            "skeema_1.13.2_mac_arm64.tar.gz",
            "skeema_1.13.2_mac_arm64.zip",
            "skeema_amd64.apk",
            "skeema_amd64.deb",
            "skeema_amd64.rpm",
            "skeema_arm64.apk",
            "skeema_arm64.deb",
            "skeema_arm64.rpm",
            "skeema_checksums_1.13.2.txt",
        ]
    }

    #[test]
    fn test_skeema_skeema_skeema_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 4),
            ],
            &skeema_skeema_skeema_names(),
            "skeema",
        );
    }

    fn skim_rs_skim_skim_names() -> Vec<&'static str> {
        vec![
            "dist-manifest.json",
            "sha256.sum",
            "skim-aarch64-apple-darwin.tar.xz",
            "skim-aarch64-apple-darwin.tar.xz.sha256",
            "skim-aarch64-unknown-linux-gnu.tar.xz",
            "skim-aarch64-unknown-linux-gnu.tar.xz.sha256",
            "skim-installer.sh",
            "skim-x86_64-apple-darwin.tar.xz",
            "skim-x86_64-apple-darwin.tar.xz.sha256",
            "skim-x86_64-unknown-linux-gnu.tar.xz",
            "skim-x86_64-unknown-linux-gnu.tar.xz.sha256",
            "skim-x86_64-unknown-linux-musl.tar.xz",
            "skim-x86_64-unknown-linux-musl.tar.xz.sha256",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_skim_rs_skim_skim_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 11),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 2),
            ],
            &skim_rs_skim_skim_names(),
            "skim",
        );
    }

    fn smithy_lang_smithy_smithy_cli_names() -> Vec<&'static str> {
        vec![
            "smithy-cli-darwin-aarch64.zip",
            "smithy-cli-darwin-aarch64.zip.asc",
            "smithy-cli-darwin-aarch64.zip.sha256",
            "smithy-cli-darwin-x86_64.zip",
            "smithy-cli-darwin-x86_64.zip.asc",
            "smithy-cli-darwin-x86_64.zip.sha256",
            "smithy-cli-linux-aarch64.zip",
            "smithy-cli-linux-aarch64.zip.asc",
            "smithy-cli-linux-aarch64.zip.sha256",
            "smithy-cli-linux-x86_64.zip",
            "smithy-cli-linux-x86_64.zip.asc",
            "smithy-cli-linux-x86_64.zip.sha256",
            "smithy-cli-windows-x64.zip",
            "smithy-cli-windows-x64.zip.asc",
            "smithy-cli-windows-x64.zip.sha256",
        ]
    }

    #[test]
    fn test_smithy_lang_smithy_smithy_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 12),
            ],
            &smithy_lang_smithy_smithy_cli_names(),
            "smithy-cli",
        );
    }

    fn snyk_cli_snyk_names() -> Vec<&'static str> {
        vec![
            "ls-protocol-version-23",
            "sha256sums.txt.asc",
            "snyk-alpine",
            "snyk-alpine-arm64",
            "snyk-alpine-arm64.sha256",
            "snyk-alpine.sha256",
            "snyk-linux",
            "snyk-linux-arm64",
            "snyk-linux-arm64.sha256",
            "snyk-linux.sha256",
            "snyk-macos",
            "snyk-macos-arm64",
            "snyk-macos-arm64.sha256",
            "snyk-macos.sha256",
            "snyk-win.exe",
            "snyk-win.exe.sha256",
        ]
    }

    #[test]
    fn test_snyk_cli_snyk_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 6),
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 7),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 11),
            ],
            &snyk_cli_snyk_names(),
            "snyk",
        );
    }

    fn so_dang_cool_dt_dt_names() -> Vec<&'static str> {
        vec![
            "dt-aarch64-linux-gnu.tgz",
            "dt-aarch64-linux-musleabi.tgz",
            "dt-aarch64-macos-none.tgz",
            "dt-arm-linux-musleabi.tgz",
            "dt-arm-linux-musleabihf.tgz",
            "dt-mips-linux-gnu.tgz",
            "dt-mips-linux-musl.tgz",
            "dt-mips64-linux-gnuabi64.tgz",
            "dt-mips64-linux-musl.tgz",
            "dt-mips64el-linux-gnuabi64.tgz",
            "dt-mips64el-linux-musl.tgz",
            "dt-mipsel-linux-gnu.tgz",
            "dt-mipsel-linux-musl.tgz",
            "dt-powerpc-linux-gnu.tgz",
            "dt-powerpc-linux-musl.tgz",
            "dt-powerpc64le-linux-gnu.tgz",
            "dt-powerpc64le-linux-musl.tgz",
            "dt-riscv64-linux-gnu.tgz",
            "dt-riscv64-linux-musl.tgz",
            "dt-wasm32-wasi-musl.tgz",
            "dt-x86-linux-gnu.tgz",
            "dt-x86-linux-musl.tgz",
            "dt-x86-windows-gnu.tgz",
            "dt-x86_64-linux-gnu.tgz",
            "dt-x86_64-linux-musl.tgz",
            "dt-x86_64-macos-none.tgz",
            "dt-x86_64-windows-gnu.tgz",
        ]
    }

    #[test]
    fn test_so_dang_cool_dt_dt_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 23),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 25),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 26),
            ],
            &so_dang_cool_dt_dt_names(),
            "dt",
        );
    }

    fn so_dang_cool_fib_fib_names() -> Vec<&'static str> {
        vec![
            "fib-x86_64-linux.tgz",
        ]
    }

    #[test]
    fn test_so_dang_cool_fib_fib_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
            ],
            &so_dang_cool_fib_fib_names(),
            "fib",
        );
    }

    fn so_dang_cool_findup_findup_names() -> Vec<&'static str> {
        vec![
            "findup-aarch64-linux-gnu.tgz",
            "findup-aarch64-linux-musleabi.tgz",
            "findup-aarch64-macos-none.tgz",
            "findup-arm-linux-musleabi.tgz",
            "findup-arm-linux-musleabihf.tgz",
            "findup-mips-linux-gnu.tgz",
            "findup-mips-linux-musl.tgz",
            "findup-mips64-linux-gnuabi64.tgz",
            "findup-mips64-linux-musl.tgz",
            "findup-mips64el-linux-gnuabi64.tgz",
            "findup-mips64el-linux-musl.tgz",
            "findup-mipsel-linux-gnu.tgz",
            "findup-mipsel-linux-musl.tgz",
            "findup-powerpc-linux-gnu.tgz",
            "findup-powerpc-linux-musl.tgz",
            "findup-powerpc64le-linux-gnu.tgz",
            "findup-powerpc64le-linux-musl.tgz",
            "findup-riscv64-linux-gnu.tgz",
            "findup-riscv64-linux-musl.tgz",
            "findup-x86-linux-gnu.tgz",
            "findup-x86-linux-musl.tgz",
            "findup-x86_64-linux-gnu.tgz",
            "findup-x86_64-linux-musl.tgz",
            "findup-x86_64-macos-none.tgz",
        ]
    }

    #[test]
    fn test_so_dang_cool_findup_findup_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 21),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 23),
                (Platform::OsxArm64, 2),
            ],
            &so_dang_cool_findup_findup_names(),
            "findup",
        );
    }

    fn sorah_mairu_mairu_names() -> Vec<&'static str> {
        vec![
            "mairu-aarch64-apple-darwin.tar.gz",
            "mairu-aarch64-unknown-linux-musl.tar.gz",
            "mairu-universal-apple-darwin.tar.gz",
            "mairu-x86_64-unknown-linux-musl.tar.gz",
            "mairu_0.11.0-1_amd64.deb",
            "mairu_0.11.0-1_arm64.deb",
        ]
    }

    #[test]
    fn test_sorah_mairu_mairu_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 0),
            ],
            &sorah_mairu_mairu_names(),
            "mairu",
        );
    }

    fn sourcemeta_jsonschema_jsonschema_names() -> Vec<&'static str> {
        vec![
            "CHECKSUMS.txt",
            "CHECKSUMS.txt.asc",
            "jsonschema-14.13.4-darwin-arm64.zip",
            "jsonschema-14.13.4-darwin-x86_64.zip",
            "jsonschema-14.13.4-linux-arm64.zip",
            "jsonschema-14.13.4-linux-x86_64-musl.zip",
            "jsonschema-14.13.4-linux-x86_64.zip",
            "jsonschema-14.13.4-windows-x86_64.zip",
            "jsonschema_14.13.4_amd64.snap",
            "jsonschema_14.13.4_arm64.snap",
        ]
    }

    #[test]
    fn test_sourcemeta_jsonschema_jsonschema_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 3),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 7),
            ],
            &sourcemeta_jsonschema_jsonschema_names(),
            "jsonschema",
        );
    }

    fn spinel_coop_rv_rv_names() -> Vec<&'static str> {
        vec![
            "dist-manifest.json",
            "rv-aarch64-apple-darwin.tar.xz",
            "rv-aarch64-apple-darwin.tar.xz.sha256",
            "rv-aarch64-unknown-linux-gnu.tar.xz",
            "rv-aarch64-unknown-linux-gnu.tar.xz.sha256",
            "rv-aarch64-unknown-linux-musl.tar.xz",
            "rv-aarch64-unknown-linux-musl.tar.xz.sha256",
            "rv-installer.ps1",
            "rv-installer.sh",
            "rv-x86_64-apple-darwin.tar.xz",
            "rv-x86_64-apple-darwin.tar.xz.sha256",
            "rv-x86_64-pc-windows-msvc.zip",
            "rv-x86_64-pc-windows-msvc.zip.sha256",
            "rv-x86_64-unknown-linux-gnu.tar.xz",
            "rv-x86_64-unknown-linux-gnu.tar.xz.sha256",
            "rv-x86_64-unknown-linux-musl.tar.xz",
            "rv-x86_64-unknown-linux-musl.tar.xz.sha256",
            "sha256.sum",
            "source.tar.gz",
            "source.tar.gz.sha256",
        ]
    }

    #[test]
    fn test_spinel_coop_rv_rv_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 13),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 1),
            ],
            &spinel_coop_rv_rv_names(),
            "rv",
        );
    }

    fn spotdl_spotify_downloader_spotdl_names() -> Vec<&'static str> {
        vec![
            "spotDL",
            "spotdl-4.4.3-darwin",
            "spotdl-4.4.3-linux",
            "spotdl-4.4.3-win32.exe",
        ]
    }

    #[test]
    fn test_spotdl_spotify_downloader_spotdl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
            ],
            &spotdl_spotify_downloader_spotdl_names(),
            "spotdl",
        );
    }

    fn sqls_server_sqls_sqls_names() -> Vec<&'static str> {
        vec![
            "sqls-darwin-0.2.45.zip",
            "sqls-linux-0.2.45.zip",
            "sqls-windows-0.2.45.zip",
        ]
    }

    #[test]
    fn test_sqls_server_sqls_sqls_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 0),
                (Platform::Win32, 2),
                (Platform::Win64, 2),
                (Platform::WinArm64, 2),
            ],
            &sqls_server_sqls_sqls_names(),
            "sqls",
        );
    }

    fn sstadick_crabz_crabz_names() -> Vec<&'static str> {
        vec![
            "crabz-linux-amd64",
            "crabz-linux-amd64-src.tar.gz",
            "crabz-linux-amd64.deb",
            "crabz-macos-amd64",
            "crabz-macos-amd64-src.tar.gz",
            "crabz-windows-amd64.exe",
            "crabz-windows-amd64.exe-src.zip",
        ]
    }

    #[test]
    fn test_sstadick_crabz_crabz_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 3),
            ],
            &sstadick_crabz_crabz_names(),
            "crabz",
        );
    }

    fn str4d_age_plugin_yubikey_age_plugin_yubikey_names() -> Vec<&'static str> {
        vec![
            "age-plugin-yubikey-v0.5.0-arm64-darwin.tar.gz",
            "age-plugin-yubikey-v0.5.0-x86_64-linux.tar.gz",
            "age-plugin-yubikey-v0.5.0-x86_64-windows.zip",
            "age-plugin-yubikey_0.5.0-1_amd64.deb",
        ]
    }

    #[test]
    fn test_str4d_age_plugin_yubikey_age_plugin_yubikey_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 2),
            ],
            &str4d_age_plugin_yubikey_age_plugin_yubikey_names(),
            "age-plugin-yubikey",
        );
    }

    fn str4d_rage_rage_names() -> Vec<&'static str> {
        vec![
            "rage-musl_0.11.1-1_amd64.deb",
            "rage-musl_0.11.1-1_arm64.deb",
            "rage-musl_0.11.1-1_armhf.deb",
            "rage-v0.11.1-arm64-darwin.tar.gz",
            "rage-v0.11.1-arm64-linux.tar.gz",
            "rage-v0.11.1-armv7-linux.tar.gz",
            "rage-v0.11.1-x86_64-darwin.tar.gz",
            "rage-v0.11.1-x86_64-linux.tar.gz",
            "rage-v0.11.1-x86_64-windows.zip",
            "rage_0.11.1-1_amd64.deb",
            "rage_0.11.1-1_arm64.deb",
            "rage_0.11.1-1_armhf.deb",
        ]
    }

    #[test]
    fn test_str4d_rage_rage_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 8),
            ],
            &str4d_rage_rage_names(),
            "rage",
        );
    }

    fn stripe_stripe_cli_stripe_names() -> Vec<&'static str> {
        vec![
            "stripe-linux-checksums.txt",
            "stripe-mac-checksums.txt",
            "stripe-windows-checksums.txt",
            "stripe_1.37.2_linux_amd64.deb",
            "stripe_1.37.2_linux_amd64.rpm",
            "stripe_1.37.2_linux_arm64.deb",
            "stripe_1.37.2_linux_arm64.rpm",
            "stripe_1.37.2_linux_arm64.tar.gz",
            "stripe_1.37.2_linux_x86_64.tar.gz",
            "stripe_1.37.2_mac-os_arm64.tar.gz",
            "stripe_1.37.2_mac-os_x86_64.tar.gz",
            "stripe_1.37.2_windows_i386.zip",
            "stripe_1.37.2_windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_stripe_stripe_cli_stripe_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::LinuxAarch64, 7),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 9),
                (Platform::Win64, 12),
            ],
            &stripe_stripe_cli_stripe_names(),
            "stripe",
        );
    }

    fn stunnel_static_curl_curl_names() -> Vec<&'static str> {
        vec![
            "curl-linux-aarch64-dev-8.18.0.tar.xz",
            "curl-linux-aarch64-glibc-8.18.0.tar.xz",
            "curl-linux-aarch64-musl-8.18.0.tar.xz",
            "curl-linux-armv5-dev-8.18.0.tar.xz",
            "curl-linux-armv5-glibc-8.18.0.tar.xz",
            "curl-linux-armv5-musl-8.18.0.tar.xz",
            "curl-linux-armv7-dev-8.18.0.tar.xz",
            "curl-linux-armv7-glibc-8.18.0.tar.xz",
            "curl-linux-armv7-musl-8.18.0.tar.xz",
            "curl-linux-i686-dev-8.18.0.tar.xz",
            "curl-linux-i686-glibc-8.18.0.tar.xz",
            "curl-linux-i686-musl-8.18.0.tar.xz",
            "curl-linux-loongarch64-dev-8.18.0.tar.xz",
            "curl-linux-loongarch64-glibc-8.18.0.tar.xz",
            "curl-linux-loongarch64-musl-8.18.0.tar.xz",
            "curl-linux-mips-dev-8.18.0.tar.xz",
            "curl-linux-mips-glibc-8.18.0.tar.xz",
            "curl-linux-mips-musl-8.18.0.tar.xz",
            "curl-linux-mips64-dev-8.18.0.tar.xz",
            "curl-linux-mips64-glibc-8.18.0.tar.xz",
            "curl-linux-mips64-musl-8.18.0.tar.xz",
            "curl-linux-mips64el-dev-8.18.0.tar.xz",
            "curl-linux-mips64el-glibc-8.18.0.tar.xz",
            "curl-linux-mips64el-musl-8.18.0.tar.xz",
            "curl-linux-mipsel-dev-8.18.0.tar.xz",
            "curl-linux-mipsel-glibc-8.18.0.tar.xz",
            "curl-linux-mipsel-musl-8.18.0.tar.xz",
            "curl-linux-powerpc-dev-8.18.0.tar.xz",
            "curl-linux-powerpc-glibc-8.18.0.tar.xz",
            "curl-linux-powerpc-musl-8.18.0.tar.xz",
            "curl-linux-powerpc64le-dev-8.18.0.tar.xz",
            "curl-linux-powerpc64le-glibc-8.18.0.tar.xz",
            "curl-linux-powerpc64le-musl-8.18.0.tar.xz",
            "curl-linux-riscv64-dev-8.18.0.tar.xz",
            "curl-linux-riscv64-glibc-8.18.0.tar.xz",
            "curl-linux-riscv64-musl-8.18.0.tar.xz",
            "curl-linux-s390x-dev-8.18.0.tar.xz",
            "curl-linux-s390x-glibc-8.18.0.tar.xz",
            "curl-linux-s390x-musl-8.18.0.tar.xz",
            "curl-linux-x86_64-dev-8.18.0.tar.xz",
            "curl-linux-x86_64-glibc-8.18.0.tar.xz",
            "curl-linux-x86_64-musl-8.18.0.tar.xz",
            "curl-macos-arm64-8.18.0.tar.xz",
            "curl-macos-arm64-dev-8.18.0.tar.xz",
            "curl-macos-x86_64-8.18.0.tar.xz",
            "curl-macos-x86_64-dev-8.18.0.tar.xz",
            "curl-windows-aarch64-8.18.0.tar.xz",
            "curl-windows-aarch64-dev-8.18.0.tar.xz",
            "curl-windows-armv7-8.18.0.tar.xz",
            "curl-windows-armv7-dev-8.18.0.tar.xz",
            "curl-windows-i686-8.18.0.tar.xz",
            "curl-windows-i686-dev-8.18.0.tar.xz",
            "curl-windows-x86_64-8.18.0.tar.xz",
            "curl-windows-x86_64-dev-8.18.0.tar.xz",
        ]
    }

    #[test]
    fn test_stunnel_static_curl_curl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 39),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 45),
                (Platform::OsxArm64, 43),
                (Platform::Win64, 53),
                (Platform::WinArm64, 47),
            ],
            &stunnel_static_curl_curl_names(),
            "curl",
        );
    }

    fn svenstaro_genact_genact_names() -> Vec<&'static str> {
        vec![
            "genact-1.5.1-aarch64-unknown-linux-gnu",
            "genact-1.5.1-aarch64-unknown-linux-musl",
            "genact-1.5.1-arm-unknown-linux-musleabihf",
            "genact-1.5.1-armv7-unknown-linux-gnueabihf",
            "genact-1.5.1-armv7-unknown-linux-musleabihf",
            "genact-1.5.1-x86_64-apple-darwin",
            "genact-1.5.1-x86_64-pc-windows-msvc.exe",
            "genact-1.5.1-x86_64-unknown-freebsd",
            "genact-1.5.1-x86_64-unknown-linux-gnu",
            "genact-1.5.1-x86_64-unknown-linux-musl",
        ]
    }

    #[test]
    fn test_svenstaro_genact_genact_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 9),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 5),
            ],
            &svenstaro_genact_genact_names(),
            "genact",
        );
    }

    fn svenstaro_miniserve_miniserve_names() -> Vec<&'static str> {
        vec![
            "miniserve-0.33.0-aarch64-apple-darwin",
            "miniserve-0.33.0-aarch64-unknown-linux-gnu",
            "miniserve-0.33.0-aarch64-unknown-linux-musl",
            "miniserve-0.33.0-arm-unknown-linux-musleabihf",
            "miniserve-0.33.0-armv7-unknown-linux-gnueabihf",
            "miniserve-0.33.0-armv7-unknown-linux-musleabihf",
            "miniserve-0.33.0-i686-pc-windows-msvc.exe",
            "miniserve-0.33.0-riscv64gc-unknown-linux-gnu",
            "miniserve-0.33.0-x86_64-apple-darwin",
            "miniserve-0.33.0-x86_64-pc-windows-msvc.exe",
            "miniserve-0.33.0-x86_64-unknown-freebsd",
            "miniserve-0.33.0-x86_64-unknown-illumos",
            "miniserve-0.33.0-x86_64-unknown-linux-gnu",
            "miniserve-0.33.0-x86_64-unknown-linux-musl",
        ]
    }

    #[test]
    fn test_svenstaro_miniserve_miniserve_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 13),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 8),
                (Platform::OsxArm64, 0),
            ],
            &svenstaro_miniserve_miniserve_names(),
            "miniserve",
        );
    }

    fn swaggo_swag_swag_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "swag_2.0.0-rc5_Darwin_arm64.tar.gz",
            "swag_2.0.0-rc5_Darwin_x86_64.tar.gz",
            "swag_2.0.0-rc5_Linux_arm64.tar.gz",
            "swag_2.0.0-rc5_Linux_i386.tar.gz",
            "swag_2.0.0-rc5_Linux_x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_swaggo_swag_swag_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
            ],
            &swaggo_swag_swag_names(),
            "swag",
        );
    }

    fn sxyazi_yazi_yazi_names() -> Vec<&'static str> {
        vec![
            "yazi-aarch64-apple-darwin.zip",
            "yazi-aarch64-pc-windows-msvc.zip",
            "yazi-aarch64-unknown-linux-gnu.deb",
            "yazi-aarch64-unknown-linux-gnu.zip",
            "yazi-aarch64-unknown-linux-musl.deb",
            "yazi-aarch64-unknown-linux-musl.zip",
            "yazi-amd64.snap",
            "yazi-arm64.snap",
            "yazi-i686-unknown-linux-gnu.zip",
            "yazi-riscv64gc-unknown-linux-gnu.zip",
            "yazi-sparc64-unknown-linux-gnu.zip",
            "yazi-x86_64-apple-darwin.zip",
            "yazi-x86_64-pc-windows-msvc.zip",
            "yazi-x86_64-unknown-linux-gnu.deb",
            "yazi-x86_64-unknown-linux-gnu.zip",
            "yazi-x86_64-unknown-linux-musl.deb",
            "yazi-x86_64-unknown-linux-musl.zip",
        ]
    }

    #[test]
    fn test_sxyazi_yazi_yazi_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 16),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 11),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 12),
                (Platform::WinArm64, 1),
            ],
            &sxyazi_yazi_yazi_names(),
            "yazi",
        );
    }

    fn syncthing_syncthing_syncthing_names() -> Vec<&'static str> {
        vec![
            "compat.json",
            "sha1sum.txt.asc",
            "sha256sum.txt.asc",
            "syncthing-freebsd-amd64-v2.0.15.tar.gz",
            "syncthing-freebsd-arm64-v2.0.15.tar.gz",
            "syncthing-illumos-amd64-v2.0.15.tar.gz",
            "syncthing-linux-386-v2.0.15.tar.gz",
            "syncthing-linux-amd64-v2.0.15.tar.gz",
            "syncthing-linux-arm-v2.0.15.tar.gz",
            "syncthing-linux-arm64-v2.0.15.tar.gz",
            "syncthing-linux-loong64-v2.0.15.tar.gz",
            "syncthing-linux-mips-v2.0.15.tar.gz",
            "syncthing-linux-mips64-v2.0.15.tar.gz",
            "syncthing-linux-mips64le-v2.0.15.tar.gz",
            "syncthing-linux-mipsle-v2.0.15.tar.gz",
            "syncthing-linux-ppc64le-v2.0.15.tar.gz",
            "syncthing-linux-riscv64-v2.0.15.tar.gz",
            "syncthing-linux-s390x-v2.0.15.tar.gz",
            "syncthing-macos-amd64-v2.0.15.zip",
            "syncthing-macos-arm64-v2.0.15.zip",
            "syncthing-macos-universal-v2.0.15.zip",
            "syncthing-openbsd-amd64-v2.0.15.tar.gz",
            "syncthing-openbsd-arm64-v2.0.15.tar.gz",
            "syncthing-source-v2.0.15.tar.gz",
            "syncthing-source-v2.0.15.tar.gz.asc",
            "syncthing-windows-386-v2.0.15.zip",
            "syncthing-windows-amd64-v2.0.15.zip",
            "syncthing-windows-arm64-v2.0.15.zip",
            "syncthing_2.0.15_amd64.deb",
            "syncthing_2.0.15_arm64.deb",
            "syncthing_2.0.15_armel.deb",
            "syncthing_2.0.15_armhf.deb",
        ]
    }

    #[test]
    fn test_syncthing_syncthing_syncthing_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 6),
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 9),
                (Platform::Osx64, 18),
                (Platform::OsxArm64, 19),
                (Platform::Win32, 25),
                (Platform::Win64, 26),
                (Platform::WinArm64, 27),
            ],
            &syncthing_syncthing_syncthing_names(),
            "syncthing",
        );
    }

    fn syumai_sbx_sbx_names() -> Vec<&'static str> {
        vec![
            "sbx-amd64-darwin",
            "sbx-arm64-darwin",
            "sbx_0.0.5_checksums.txt",
        ]
    }

    #[test]
    fn test_syumai_sbx_sbx_names() {
        platform_match_test(
            &[
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 1),
            ],
            &syumai_sbx_sbx_names(),
            "sbx",
        );
    }

    fn taiki_e_cargo_llvm_cov_cargo_llvm_cov_names() -> Vec<&'static str> {
        vec![
            "cargo-llvm-cov-aarch64-apple-darwin.tar.gz",
            "cargo-llvm-cov-aarch64-pc-windows-msvc.tar.gz",
            "cargo-llvm-cov-aarch64-pc-windows-msvc.zip",
            "cargo-llvm-cov-aarch64-unknown-linux-gnu.tar.gz",
            "cargo-llvm-cov-aarch64-unknown-linux-musl.tar.gz",
            "cargo-llvm-cov-powerpc64le-unknown-linux-gnu.tar.gz",
            "cargo-llvm-cov-powerpc64le-unknown-linux-musl.tar.gz",
            "cargo-llvm-cov-riscv64gc-unknown-linux-gnu.tar.gz",
            "cargo-llvm-cov-riscv64gc-unknown-linux-musl.tar.gz",
            "cargo-llvm-cov-s390x-unknown-linux-gnu.tar.gz",
            "cargo-llvm-cov-universal-apple-darwin.tar.gz",
            "cargo-llvm-cov-x86_64-apple-darwin.tar.gz",
            "cargo-llvm-cov-x86_64-pc-windows-msvc.tar.gz",
            "cargo-llvm-cov-x86_64-pc-windows-msvc.zip",
            "cargo-llvm-cov-x86_64-unknown-freebsd.tar.gz",
            "cargo-llvm-cov-x86_64-unknown-linux-gnu.tar.gz",
            "cargo-llvm-cov-x86_64-unknown-linux-musl.tar.gz",
        ]
    }

    #[test]
    fn test_taiki_e_cargo_llvm_cov_cargo_llvm_cov_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 16),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 11),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 12),
                (Platform::WinArm64, 1),
            ],
            &taiki_e_cargo_llvm_cov_cargo_llvm_cov_names(),
            "cargo-llvm-cov",
        );
    }

    fn tailor_platform_tailorctl_tailorctl_names() -> Vec<&'static str> {
        vec![
            "tailorctl_darwin_v2.9.0_arm64.tar.gz",
            "tailorctl_darwin_v2.9.0_x86_64.tar.gz",
            "tailorctl_linux_v2.9.0_arm64.tar.gz",
            "tailorctl_linux_v2.9.0_x86_64.tar.gz",
            "tailorctl_windows_v2.9.0_x86_64.zip",
        ]
    }

    #[test]
    fn test_tailor_platform_tailorctl_tailorctl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 4),
            ],
            &tailor_platform_tailorctl_tailorctl_names(),
            "tailorctl",
        );
    }

    fn tailwindlabs_tailwindcss_tailwindcss_names() -> Vec<&'static str> {
        vec![
            "sha256sums.txt",
            "tailwindcss-linux-arm64",
            "tailwindcss-linux-arm64-musl",
            "tailwindcss-linux-x64",
            "tailwindcss-linux-x64-musl",
            "tailwindcss-macos-arm64",
            "tailwindcss-macos-x64",
            "tailwindcss-windows-x64.exe",
        ]
    }

    #[test]
    fn test_tailwindlabs_tailwindcss_tailwindcss_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 4),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 5),
            ],
            &tailwindlabs_tailwindcss_tailwindcss_names(),
            "tailwindcss",
        );
    }

    fn tealdeer_rs_tealdeer_tealdeer_names() -> Vec<&'static str> {
        vec![
            "completions_bash",
            "completions_fish",
            "completions_zsh",
            "LICENSE-APACHE.txt",
            "LICENSE-MIT.txt",
            "tealdeer-linux-aarch64-musl",
            "tealdeer-linux-aarch64-musl.sha256",
            "tealdeer-linux-arm-musleabi",
            "tealdeer-linux-arm-musleabi.sha256",
            "tealdeer-linux-arm-musleabihf",
            "tealdeer-linux-arm-musleabihf.sha256",
            "tealdeer-linux-armv7-musleabihf",
            "tealdeer-linux-armv7-musleabihf.sha256",
            "tealdeer-linux-i686-musl",
            "tealdeer-linux-i686-musl.sha256",
            "tealdeer-linux-x86_64-musl",
            "tealdeer-linux-x86_64-musl.sha256",
            "tealdeer-macos-aarch64",
            "tealdeer-macos-aarch64.sha256",
            "tealdeer-macos-x86_64",
            "tealdeer-macos-x86_64.sha256",
            "tealdeer-windows-x86_64-msvc.exe",
            "tealdeer-windows-x86_64-msvc.exe.sha256",
        ]
    }

    #[test]
    fn test_tealdeer_rs_tealdeer_tealdeer_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 15),
                (Platform::LinuxAarch64, 5),
                (Platform::Osx64, 19),
                (Platform::OsxArm64, 17),
            ],
            &tealdeer_rs_tealdeer_tealdeer_names(),
            "tealdeer",
        );
    }

    fn technicalpickles_envsense_envsense_names() -> Vec<&'static str> {
        vec![
            "envsense-0.6.0-aarch64-unknown-linux-gnu",
            "envsense-0.6.0-aarch64-unknown-linux-gnu.bundle",
            "envsense-0.6.0-aarch64-unknown-linux-gnu.sha256",
            "envsense-0.6.0-aarch64-unknown-linux-gnu.sig",
            "envsense-0.6.0-universal-apple-darwin",
            "envsense-0.6.0-universal-apple-darwin.bundle",
            "envsense-0.6.0-universal-apple-darwin.sha256",
            "envsense-0.6.0-universal-apple-darwin.sig",
            "envsense-0.6.0-x86_64-unknown-linux-gnu",
            "envsense-0.6.0-x86_64-unknown-linux-gnu.bundle",
            "envsense-0.6.0-x86_64-unknown-linux-gnu.sha256",
            "envsense-0.6.0-x86_64-unknown-linux-gnu.sig",
        ]
    }

    #[test]
    fn test_technicalpickles_envsense_envsense_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 8),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 4),
            ],
            &technicalpickles_envsense_envsense_names(),
            "envsense",
        );
    }

    fn tektoncd_cli_tkn_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "tektoncd-cli-0.44.0_Linux-64bit.deb",
            "tektoncd-cli-0.44.0_Linux-64bit.rpm",
            "tektoncd-cli-0.44.0_Linux-ARM64.deb",
            "tektoncd-cli-0.44.0_Linux-ARM64.rpm",
            "tektoncd-cli-0.44.0_Linux-ppc64le.deb",
            "tektoncd-cli-0.44.0_Linux-ppc64le.rpm",
            "tektoncd-cli-0.44.0_Linux-s390x.deb",
            "tektoncd-cli-0.44.0_Linux-s390x.rpm",
            "tkn_0.44.0_Darwin_all.tar.gz",
            "tkn_0.44.0_Linux_aarch64.tar.gz",
            "tkn_0.44.0_Linux_ppc64le.tar.gz",
            "tkn_0.44.0_Linux_s390x.tar.gz",
            "tkn_0.44.0_Linux_x86_64.tar.gz",
            "tkn_0.44.0_Windows_aarch64.zip",
            "tkn_0.44.0_Windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_tektoncd_cli_tkn_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 13),
                (Platform::LinuxAarch64, 10),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 9),
                (Platform::Win64, 15),
                (Platform::WinArm64, 14),
            ],
            &tektoncd_cli_tkn_names(),
            "tkn",
        );
    }

    fn tellerops_teller_teller_names() -> Vec<&'static str> {
        vec![
            "teller-aarch64-macos.tar.xz",
            "teller-x86_64-linux.tar.xz",
            "teller-x86_64-macos.tar.xz",
            "teller-x86_64-windows.zip",
        ]
    }

    #[test]
    fn test_tellerops_teller_teller_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 3),
            ],
            &tellerops_teller_teller_names(),
            "teller",
        );
    }

    fn tenable_terrascan_terrascan_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "terrascan_1.19.9_Darwin_arm64.tar.gz",
            "terrascan_1.19.9_Darwin_x86_64.tar.gz",
            "terrascan_1.19.9_Linux_arm64.tar.gz",
            "terrascan_1.19.9_Linux_i386.tar.gz",
            "terrascan_1.19.9_Linux_x86_64.tar.gz",
            "terrascan_1.19.9_Windows_i386.tar.gz",
            "terrascan_1.19.9_Windows_i386.zip",
            "terrascan_1.19.9_Windows_x86_64.tar.gz",
            "terrascan_1.19.9_Windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_tenable_terrascan_terrascan_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 4),
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 3),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 1),
                (Platform::Win64, 9),
            ],
            &tenable_terrascan_terrascan_names(),
            "terrascan",
        );
    }

    fn terrastruct_d2_d2_names() -> Vec<&'static str> {
        vec![
            "d2-v0.7.1-linux-amd64.tar.gz",
            "d2-v0.7.1-linux-arm64.tar.gz",
            "d2-v0.7.1-macos-amd64.tar.gz",
            "d2-v0.7.1-macos-arm64.tar.gz",
            "d2-v0.7.1-windows-amd64.msi",
            "d2-v0.7.1-windows-amd64.tar.gz",
            "d2-v0.7.1-windows-arm64.tar.gz",
        ]
    }

    #[test]
    fn test_terrastruct_d2_d2_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 5),
                (Platform::WinArm64, 6),
            ],
            &terrastruct_d2_d2_names(),
            "d2",
        );
    }

    fn theryangeary_choose_choose_names() -> Vec<&'static str> {
        vec![
            "choose-aarch64-apple-darwin",
            "choose-aarch64-unknown-linux-gnu",
            "choose-x86_64-pc-windows-gnu",
            "choose-x86_64-pc-windows-gnu.exe",
            "choose-x86_64-unknown-linux-gnu",
            "choose-x86_64-unknown-linux-musl",
        ]
    }

    #[test]
    fn test_theryangeary_choose_choose_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 5),
                (Platform::LinuxAarch64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 2),
            ],
            &theryangeary_choose_choose_names(),
            "choose",
        );
    }

    fn tilt_dev_ctlptl_ctlptl_names() -> Vec<&'static str> {
        vec![
            "checksums.txt",
            "ctlptl.0.9.0.linux.arm64.tar.gz",
            "ctlptl.0.9.0.linux.x86_64.tar.gz",
            "ctlptl.0.9.0.mac.arm64.tar.gz",
            "ctlptl.0.9.0.mac.x86_64.tar.gz",
            "ctlptl.0.9.0.windows.x86_64.zip",
        ]
    }

    #[test]
    fn test_tilt_dev_ctlptl_ctlptl_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 3),
                (Platform::Win64, 5),
            ],
            &tilt_dev_ctlptl_ctlptl_names(),
            "ctlptl",
        );
    }

    fn timvisee_ffsend_ffsend_names() -> Vec<&'static str> {
        vec![
            "ffsend-v0.2.77-linux-x64",
            "ffsend-v0.2.77-linux-x64-static",
        ]
    }

    #[test]
    fn test_timvisee_ffsend_ffsend_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
            ],
            &timvisee_ffsend_ffsend_names(),
            "ffsend",
        );
    }

    fn tombi_toml_tombi_tombi_cli_names() -> Vec<&'static str> {
        vec![
            "tombi-cli-0.9.0-aarch64-apple-darwin.gz",
            "tombi-cli-0.9.0-aarch64-pc-windows-msvc.zip",
            "tombi-cli-0.9.0-aarch64-unknown-linux-musl.gz",
            "tombi-cli-0.9.0-arm-unknown-linux-gnueabihf.gz",
            "tombi-cli-0.9.0-x86_64-apple-darwin.gz",
            "tombi-cli-0.9.0-x86_64-pc-windows-msvc.zip",
            "tombi-cli-0.9.0-x86_64-unknown-linux-musl.gz",
            "tombi-vscode-0.9.0-darwin-arm64.vsix",
            "tombi-vscode-0.9.0-darwin-x64.vsix",
            "tombi-vscode-0.9.0-linux-arm64.vsix",
            "tombi-vscode-0.9.0-linux-armhf.vsix",
            "tombi-vscode-0.9.0-linux-x64.vsix",
            "tombi-vscode-0.9.0-win32-arm64.vsix",
            "tombi-vscode-0.9.0-win32-x64.vsix",
        ]
    }

    #[test]
    fn test_tombi_toml_tombi_tombi_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 4),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 5),
                (Platform::WinArm64, 1),
            ],
            &tombi_toml_tombi_tombi_cli_names(),
            "tombi-cli",
        );
    }

    fn tree_sitter_tree_sitter_tree_sitter_names() -> Vec<&'static str> {
        vec![
            "tree-sitter-linux-arm.gz",
            "tree-sitter-linux-arm64.gz",
            "tree-sitter-linux-powerpc64.gz",
            "tree-sitter-linux-x64.gz",
            "tree-sitter-linux-x86.gz",
            "tree-sitter-macos-arm64.gz",
            "tree-sitter-macos-x64.gz",
            "tree-sitter-windows-arm64.gz",
            "tree-sitter-windows-x64.gz",
            "tree-sitter-windows-x86.gz",
            "web-tree-sitter.tar.gz",
        ]
    }

    #[test]
    fn test_tree_sitter_tree_sitter_tree_sitter_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 6),
                (Platform::OsxArm64, 5),
                (Platform::Win64, 8),
                (Platform::WinArm64, 7),
            ],
            &tree_sitter_tree_sitter_tree_sitter_names(),
            "tree-sitter",
        );
    }

    fn trzsz_trzsz_ssh_tssh_names() -> Vec<&'static str> {
        vec![
            "tssh_0.1.24_android_aarch64.tar.gz",
            "tssh_0.1.24_checksums.txt",
            "tssh_0.1.24_freebsd_aarch64.tar.gz",
            "tssh_0.1.24_freebsd_x86_64.tar.gz",
            "tssh_0.1.24_linux_aarch64.deb",
            "tssh_0.1.24_linux_aarch64.rpm",
            "tssh_0.1.24_linux_aarch64.tar.gz",
            "tssh_0.1.24_linux_armv6.deb",
            "tssh_0.1.24_linux_armv6.rpm",
            "tssh_0.1.24_linux_armv6.tar.gz",
            "tssh_0.1.24_linux_armv7.deb",
            "tssh_0.1.24_linux_armv7.rpm",
            "tssh_0.1.24_linux_armv7.tar.gz",
            "tssh_0.1.24_linux_i386.deb",
            "tssh_0.1.24_linux_i386.rpm",
            "tssh_0.1.24_linux_i386.tar.gz",
            "tssh_0.1.24_linux_loong64.deb",
            "tssh_0.1.24_linux_loong64.rpm",
            "tssh_0.1.24_linux_loong64.tar.gz",
            "tssh_0.1.24_linux_x86_64.deb",
            "tssh_0.1.24_linux_x86_64.rpm",
            "tssh_0.1.24_linux_x86_64.tar.gz",
            "tssh_0.1.24_macos_aarch64.tar.gz",
            "tssh_0.1.24_macos_x86_64.tar.gz",
            "tssh_0.1.24_win7_i386.zip",
            "tssh_0.1.24_win7_x86_64.zip",
            "tssh_0.1.24_windows_aarch64.zip",
            "tssh_0.1.24_windows_i386.zip",
            "tssh_0.1.24_windows_x86_64.zip",
        ]
    }

    #[test]
    fn test_trzsz_trzsz_ssh_tssh_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 21),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 23),
                (Platform::OsxArm64, 22),
                (Platform::Win64, 28),
                (Platform::WinArm64, 26),
            ],
            &trzsz_trzsz_ssh_tssh_names(),
            "tssh",
        );
    }

    fn tsl0922_ttyd_ttyd_names() -> Vec<&'static str> {
        vec![
            "SHA256SUMS",
            "ttyd.aarch64",
            "ttyd.arm",
            "ttyd.armhf",
            "ttyd.i686",
            "ttyd.mips",
            "ttyd.mips64",
            "ttyd.mips64el",
            "ttyd.mipsel",
            "ttyd.s390x",
            "ttyd.win32.exe",
            "ttyd.x86_64",
        ]
    }

    #[test]
    fn test_tsl0922_ttyd_ttyd_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 11),
                (Platform::LinuxAarch64, 1),
                (Platform::Win64, 11),
            ],
            &tsl0922_ttyd_ttyd_names(),
            "ttyd",
        );
    }

    fn tstack_lnav_lnav_names() -> Vec<&'static str> {
        vec![
            "lnav-0.13.2-aarch64-macos.zip",
            "lnav-0.13.2-linux-musl-arm64.zip",
            "lnav-0.13.2-linux-musl-x86_64.zip",
            "lnav-0.13.2-windows-arm64.zip",
            "lnav-0.13.2-windows-x86_64.zip",
            "lnav-0.13.2-x86_64-macos.zip",
            "lnav-0.13.2.tar.bz2",
            "lnav-0.13.2.tar.gz",
        ]
    }

    #[test]
    fn test_tstack_lnav_lnav_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 5),
                (Platform::Win64, 4),
                (Platform::WinArm64, 3),
            ],
            &tstack_lnav_lnav_names(),
            "lnav",
        );
    }

    fn typst_typst_typst_names() -> Vec<&'static str> {
        vec![
            "typst-aarch64-apple-darwin.tar.xz",
            "typst-aarch64-pc-windows-msvc.zip",
            "typst-aarch64-unknown-linux-musl.tar.xz",
            "typst-armv7-unknown-linux-musleabi.tar.xz",
            "typst-riscv64gc-unknown-linux-gnu.tar.xz",
            "typst-x86_64-apple-darwin.tar.xz",
            "typst-x86_64-pc-windows-msvc.zip",
            "typst-x86_64-unknown-linux-musl.tar.xz",
        ]
    }

    #[test]
    fn test_typst_typst_typst_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 5),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 6),
                (Platform::WinArm64, 1),
            ],
            &typst_typst_typst_names(),
            "typst",
        );
    }

    fn unfrl_dug_dug_names() -> Vec<&'static str> {
        vec![
            "dug-linux-arm64",
            "dug-linux-x64",
            "dug-osx-x64",
            "dug.0.0.94.linux-arm64.deb",
            "dug.0.0.94.linux-x64.deb",
            "dug.0.0.94.linux-x64.rpm",
            "dug.0.0.94.linux-x64.tar.gz",
            "dug.0.0.94.nupkg",
            "dug.0.0.94.osx-x64.tar.gz",
            "dug.exe",
        ]
    }

    #[test]
    fn test_unfrl_dug_dug_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 8),
            ],
            &unfrl_dug_dug_names(),
            "dug",
        );
    }

    fn upx_upx_upx_names() -> Vec<&'static str> {
        vec![
            "upx-5.1.0-amd64_linux.tar.xz",
            "upx-5.1.0-arm64_linux.tar.xz",
            "upx-5.1.0-armeb_linux.tar.xz",
            "upx-5.1.0-arm_linux.tar.xz",
            "upx-5.1.0-dos.zip",
            "upx-5.1.0-i386_linux.tar.xz",
            "upx-5.1.0-mipsel_linux.tar.xz",
            "upx-5.1.0-mips_linux.tar.xz",
            "upx-5.1.0-powerpc64le_linux.tar.xz",
            "upx-5.1.0-powerpc_linux.tar.xz",
            "upx-5.1.0-riscv64_linux.tar.xz",
            "upx-5.1.0-src.tar.xz",
            "upx-5.1.0-win32.zip",
            "upx-5.1.0-win64.zip",
        ]
    }

    #[test]
    fn test_upx_upx_upx_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 1),
                (Platform::Win64, 13),
            ],
            &upx_upx_upx_names(),
            "upx",
        );
    }

    fn urfave_gfmrun_gfmrun_names() -> Vec<&'static str> {
        vec![
            "gfmrun-darwin-amd64-v1.3.2",
            "gfmrun-linux-amd64-v1.3.2",
            "gfmrun-windows-amd64-v1.3.2.exe",
        ]
    }

    #[test]
    fn test_urfave_gfmrun_gfmrun_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
                (Platform::Osx64, 0),
            ],
            &urfave_gfmrun_gfmrun_names(),
            "gfmrun",
        );
    }

    fn visma_prodsec_confused_confused_names() -> Vec<&'static str> {
        vec![
            "confused_0.5_checksums.txt",
            "confused_0.5_checksums.txt.sig",
            "confused_0.5_freebsd_386.tar.gz",
            "confused_0.5_freebsd_amd64.tar.gz",
            "confused_0.5_freebsd_armv6.tar.gz",
            "confused_0.5_linux_386.tar.gz",
            "confused_0.5_linux_amd64.tar.gz",
            "confused_0.5_linux_arm64.tar.gz",
            "confused_0.5_linux_armv6.tar.gz",
            "confused_0.5_macOS_amd64.tar.gz",
            "confused_0.5_openbsd_386.tar.gz",
            "confused_0.5_openbsd_amd64.tar.gz",
            "confused_0.5_openbsd_arm64.tar.gz",
            "confused_0.5_openbsd_armv6.tar.gz",
            "confused_0.5_windows_386.zip",
            "confused_0.5_windows_amd64.zip",
            "confused_0.5_windows_armv6.zip",
        ]
    }

    #[test]
    fn test_visma_prodsec_confused_confused_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 5),
                (Platform::Linux64, 6),
                (Platform::LinuxAarch64, 7),
                (Platform::Osx64, 9),
                (Platform::Win32, 14),
                (Platform::Win64, 15),
            ],
            &visma_prodsec_confused_confused_names(),
            "confused",
        );
    }

    fn volta_cli_volta_volta_names() -> Vec<&'static str> {
        vec![
            "volta-2.0.2-linux-arm.tar.gz",
            "volta-2.0.2-linux.tar.gz",
            "volta-2.0.2-macos.tar.gz",
            "volta-2.0.2-windows-arm64.msi",
            "volta-2.0.2-windows-arm64.zip",
            "volta-2.0.2-windows-x86_64.msi",
            "volta-2.0.2-windows.zip",
            "volta.manifest",
        ]
    }

    #[test]
    fn test_volta_cli_volta_volta_names() {
        platform_match_test(
            &[
                (Platform::Linux32, 1),
                (Platform::Linux64, 1),
                (Platform::LinuxAarch64, 0),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 2),
                (Platform::Win32, 6),
                (Platform::Win64, 6),
                (Platform::WinArm64, 4),
            ],
            &volta_cli_volta_volta_names(),
            "volta",
        );
    }

    fn wasmerio_wapm_cli_wapm_cli_names() -> Vec<&'static str> {
        vec![
            "wapm-cli-darwin-aarch64.tar.gz",
            "wapm-cli-darwin-amd64.tar.gz",
            "wapm-cli-linux-aarch64.tar.gz",
            "wapm-cli-linux-amd64.tar.gz",
            "wapm-cli-windows-amd64.tar.gz",
        ]
    }

    #[test]
    fn test_wasmerio_wapm_cli_wapm_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 2),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 0),
                (Platform::Win64, 4),
            ],
            &wasmerio_wapm_cli_wapm_cli_names(),
            "wapm-cli",
        );
    }

    fn watchexec_cargo_watch_cargo_watch_names() -> Vec<&'static str> {
        vec![
            "B3SUMS",
            "B3SUMS.auto.minisig",
            "cargo-watch-v8.5.3-aarch64-apple-darwin.tar.xz",
            "cargo-watch-v8.5.3-aarch64-pc-windows-msvc.zip",
            "cargo-watch-v8.5.3-aarch64-unknown-linux-gnu.deb",
            "cargo-watch-v8.5.3-aarch64-unknown-linux-gnu.rpm",
            "cargo-watch-v8.5.3-aarch64-unknown-linux-gnu.tar.xz",
            "cargo-watch-v8.5.3-armv7-unknown-linux-gnueabihf.deb",
            "cargo-watch-v8.5.3-armv7-unknown-linux-gnueabihf.rpm",
            "cargo-watch-v8.5.3-armv7-unknown-linux-gnueabihf.tar.xz",
            "cargo-watch-v8.5.3-x86_64-apple-darwin.tar.xz",
            "cargo-watch-v8.5.3-x86_64-pc-windows-msvc.zip",
            "cargo-watch-v8.5.3-x86_64-unknown-linux-gnu.deb",
            "cargo-watch-v8.5.3-x86_64-unknown-linux-gnu.rpm",
            "cargo-watch-v8.5.3-x86_64-unknown-linux-gnu.tar.xz",
            "cargo-watch-v8.5.3-x86_64-unknown-linux-musl.deb",
            "cargo-watch-v8.5.3-x86_64-unknown-linux-musl.rpm",
            "cargo-watch-v8.5.3-x86_64-unknown-linux-musl.tar.xz",
            "SHA512SUMS",
            "SHA512SUMS.auto.minisig",
        ]
    }

    #[test]
    fn test_watchexec_cargo_watch_cargo_watch_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 17),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 10),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 11),
                (Platform::WinArm64, 3),
            ],
            &watchexec_cargo_watch_cargo_watch_names(),
            "cargo-watch",
        );
    }

    fn webdevops_go_crond_go_crond_names() -> Vec<&'static str> {
        vec![
            "go-crond.darwin.amd64",
            "go-crond.darwin.arm64",
            "go-crond.linux.amd64",
            "go-crond.linux.arm",
            "go-crond.linux.arm64",
        ]
    }

    #[test]
    fn test_webdevops_go_crond_go_crond_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 2),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 0),
                (Platform::OsxArm64, 1),
            ],
            &webdevops_go_crond_go_crond_names(),
            "go-crond",
        );
    }

    fn wfxr_csview_csview_names() -> Vec<&'static str> {
        vec![
            "csview-musl_1.3.4_amd64.deb",
            "csview-musl_1.3.4_arm64.deb",
            "csview-musl_1.3.4_armhf.deb",
            "csview-musl_1.3.4_i686.deb",
            "csview-v1.3.4-aarch64-apple-darwin.tar.gz",
            "csview-v1.3.4-aarch64-unknown-linux-gnu.tar.gz",
            "csview-v1.3.4-aarch64-unknown-linux-musl.tar.gz",
            "csview-v1.3.4-arm-unknown-linux-gnueabihf.tar.gz",
            "csview-v1.3.4-arm-unknown-linux-musleabihf.tar.gz",
            "csview-v1.3.4-i686-pc-windows-msvc.zip",
            "csview-v1.3.4-i686-unknown-linux-gnu.tar.gz",
            "csview-v1.3.4-i686-unknown-linux-musl.tar.gz",
            "csview-v1.3.4-x86_64-pc-windows-msvc.zip",
            "csview-v1.3.4-x86_64-unknown-linux-gnu.tar.gz",
            "csview-v1.3.4-x86_64-unknown-linux-musl.tar.gz",
            "csview_1.3.4_amd64.deb",
            "csview_1.3.4_arm64.deb",
            "csview_1.3.4_armhf.deb",
            "csview_1.3.4_i686.deb",
        ]
    }

    #[test]
    fn test_wfxr_csview_csview_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 14),
                (Platform::LinuxAarch64, 5),
                (Platform::Win64, 12),
            ],
            &wfxr_csview_csview_names(),
            "csview",
        );
    }

    fn wren_lang_wren_cli_wren_cli_names() -> Vec<&'static str> {
        vec![
            "wren-cli-linux-0.4.0.zip",
            "wren-cli-mac-0.4.0.zip",
            "wren-cli-windows-0.4.0.zip",
        ]
    }

    #[test]
    fn test_wren_lang_wren_cli_wren_cli_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 1),
                (Platform::Win32, 2),
                (Platform::Win64, 2),
                (Platform::WinArm64, 2),
            ],
            &wren_lang_wren_cli_wren_cli_names(),
            "wren-cli",
        );
    }

    fn wtetsu_gaze_gaze_names() -> Vec<&'static str> {
        vec![
            "gaze_linux_v1.2.1.zip",
            "gaze_macos_amd_v1.2.1.zip",
            "gaze_macos_arm_v1.2.1.zip",
            "gaze_windows_v1.2.1.zip",
            "license.zip",
        ]
    }

    #[test]
    fn test_wtetsu_gaze_gaze_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::Osx64, 1),
                (Platform::OsxArm64, 2),
                (Platform::Win64, 3),
            ],
            &wtetsu_gaze_gaze_names(),
            "gaze",
        );
    }

    fn xataio_pgroll_pgroll_names() -> Vec<&'static str> {
        vec![
            "pgroll.linux.amd64",
            "pgroll.linux.arm64",
            "pgroll.macos.amd64",
            "pgroll.macos.arm64",
            "pgroll.win.amd64.exe",
            "pgroll_0.16.1_checksums.txt",
        ]
    }

    #[test]
    fn test_xataio_pgroll_pgroll_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 0),
                (Platform::LinuxAarch64, 1),
                (Platform::Osx64, 2),
                (Platform::OsxArm64, 3),
            ],
            &xataio_pgroll_pgroll_names(),
            "pgroll",
        );
    }

    fn xremap_xremap_xremap_1_names() -> Vec<&'static str> {
        vec![
            "xremap-linux-aarch64-cosmic.zip",
            "xremap-linux-aarch64-ewm.zip",
            "xremap-linux-aarch64-gnome.zip",
            "xremap-linux-aarch64-hypr.zip",
            "xremap-linux-aarch64-kde.zip",
            "xremap-linux-aarch64-niri.zip",
            "xremap-linux-aarch64-socket.zip",
            "xremap-linux-aarch64-wlroots.zip",
            "xremap-linux-aarch64-x11.zip",
            "xremap-linux-x86_64-cosmic.zip",
            "xremap-linux-x86_64-ewm.zip",
            "xremap-linux-x86_64-gnome.zip",
            "xremap-linux-x86_64-hypr.zip",
            "xremap-linux-x86_64-kde.zip",
            "xremap-linux-x86_64-niri.zip",
            "xremap-linux-x86_64-socket.zip",
            "xremap-linux-x86_64-wlroots.zip",
            "xremap-linux-x86_64-x11.zip",
        ]
    }

    #[test]
    fn test_xremap_xremap_xremap_1_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 11),
            ],
            &xremap_xremap_xremap_1_names(),
            "xremap",
        );
    }

    fn xremap_xremap_xremap_2_names() -> Vec<&'static str> {
        vec![
            "xremap-linux-aarch64-cosmic.zip",
            "xremap-linux-aarch64-ewm.zip",
            "xremap-linux-aarch64-gnome.zip",
            "xremap-linux-aarch64-hypr.zip",
            "xremap-linux-aarch64-kde.zip",
            "xremap-linux-aarch64-niri.zip",
            "xremap-linux-aarch64-socket.zip",
            "xremap-linux-aarch64-wlroots.zip",
            "xremap-linux-aarch64-x11.zip",
            "xremap-linux-x86_64-cosmic.zip",
            "xremap-linux-x86_64-ewm.zip",
            "xremap-linux-x86_64-gnome.zip",
            "xremap-linux-x86_64-hypr.zip",
            "xremap-linux-x86_64-kde.zip",
            "xremap-linux-x86_64-niri.zip",
            "xremap-linux-x86_64-socket.zip",
            "xremap-linux-x86_64-wlroots.zip",
            "xremap-linux-x86_64-x11.zip",
        ]
    }

    #[test]
    fn test_xremap_xremap_xremap_2_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 13),
                (Platform::LinuxAarch64, 4),
            ],
            &xremap_xremap_xremap_2_names(),
            "xremap",
        );
    }

    fn xremap_xremap_xremap_3_names() -> Vec<&'static str> {
        vec![
            "xremap-linux-aarch64-cosmic.zip",
            "xremap-linux-aarch64-ewm.zip",
            "xremap-linux-aarch64-gnome.zip",
            "xremap-linux-aarch64-hypr.zip",
            "xremap-linux-aarch64-kde.zip",
            "xremap-linux-aarch64-niri.zip",
            "xremap-linux-aarch64-socket.zip",
            "xremap-linux-aarch64-wlroots.zip",
            "xremap-linux-aarch64-x11.zip",
            "xremap-linux-x86_64-cosmic.zip",
            "xremap-linux-x86_64-ewm.zip",
            "xremap-linux-x86_64-gnome.zip",
            "xremap-linux-x86_64-hypr.zip",
            "xremap-linux-x86_64-kde.zip",
            "xremap-linux-x86_64-niri.zip",
            "xremap-linux-x86_64-socket.zip",
            "xremap-linux-x86_64-wlroots.zip",
            "xremap-linux-x86_64-x11.zip",
        ]
    }

    #[test]
    fn test_xremap_xremap_xremap_3_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 17),
            ],
            &xremap_xremap_xremap_3_names(),
            "xremap",
        );
    }

    fn yaml_yamlscript_ys_names() -> Vec<&'static str> {
        vec![
            "libys-0.2.8-linux-aarch64.tar.xz",
            "libys-0.2.8-linux-x64.tar.xz",
            "libys-0.2.8-macos-aarch64.tar.xz",
            "libys-0.2.8-macos-x64.tar.xz",
            "libys-0.2.8-standalone.jar",
            "yamlscript.cli-0.2.8-standalone.jar",
            "ys-0.2.8-linux-aarch64.tar.xz",
            "ys-0.2.8-linux-x64.tar.xz",
            "ys-0.2.8-macos-aarch64.tar.xz",
            "ys-0.2.8-macos-x64.tar.xz",
        ]
    }

    #[test]
    fn test_yaml_yamlscript_ys_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 7),
                (Platform::LinuxAarch64, 6),
                (Platform::Osx64, 9),
                (Platform::OsxArm64, 8),
            ],
            &yaml_yamlscript_ys_names(),
            "ys",
        );
    }

    fn yujqiao_catproc_catp_names() -> Vec<&'static str> {
        vec![
            "catp-x86_64-unknown-linux-gnu.zip",
            "catp-x86_64-unknown-linux-musl.zip",
        ]
    }

    #[test]
    fn test_yujqiao_catproc_catp_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 1),
            ],
            &yujqiao_catproc_catp_names(),
            "catp",
        );
    }

    fn zk_org_zk_zk_names() -> Vec<&'static str> {
        vec![
            "zk-v0.15.2-alpine-amd64.tar.gz",
            "zk-v0.15.2-alpine-arm64.tar.gz",
            "zk-v0.15.2-alpine-i386.tar.gz",
            "zk-v0.15.2-linux-amd64.tar.gz",
            "zk-v0.15.2-linux-arm64.tar.gz",
            "zk-v0.15.2-linux-i386.tar.gz",
            "zk-v0.15.2-macos-arm64.tar.gz",
            "zk-v0.15.2-macos-x86_64.tar.gz",
            "zk-v0.15.2-windows-x86_64.tar.gz",
        ]
    }

    #[test]
    fn test_zk_org_zk_zk_names() {
        platform_match_test(
            &[
                (Platform::Linux64, 3),
                (Platform::LinuxAarch64, 4),
                (Platform::Osx64, 7),
                (Platform::OsxArm64, 6),
                (Platform::Win64, 8),
            ],
            &zk_org_zk_zk_names(),
            "zk",
        );
    }

}
