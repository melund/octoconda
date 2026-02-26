// SPDX-License-Identifier: GPL-3.0-or-later
// © Tobias Hunger <tobias.hunger@gmail.com>

use rand::random_range;

use crate::package_generation::VersionPackagingStatus;

mod cli;
mod conda;
mod config_file;
mod github;
mod package_generation;
mod types;

const PACKAGE_GENERATION_LIMIT: usize = 500;

fn report_status(
    temporary_directory: &cli::WorkDir,
    result: &[package_generation::PackageResult],
    stop_reason: &package_generation::StopReason,
) -> anyhow::Result<()> {
    let report = package_generation::report_results(result, stop_reason);
    eprintln!("{report}");

    let report = format!(
        r#"## Status

```
{report}
```

"#
    );

    std::fs::write(temporary_directory.status_file(), report.as_bytes())?;

    Ok(())
}

fn main() -> Result<(), anyhow::Error> {
    let cli = cli::parse_cli();

    let config = config_file::parse_config(&cli.config_file)?;

    let temporary_directory = cli.work_directory()?;

    package_generation::generate_build_script(temporary_directory.path())?;
    package_generation::generate_env_file(temporary_directory.path(), &config)?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let repo_packages = conda::get_conda_package_versions(
                &config.conda.full_channel()?,
                config.all_platforms().iter().copied(),
                config.packages.iter().map(|p| p.name.as_str()),
            )
            .await?;

            let gh = github::Github::new()?;

            let mut result: Vec<package_generation::PackageResult> = vec![];
            let mut package_count = 0;
            let mut stop_reason = package_generation::StopReason::Completed;

            let mut packages: Vec<_> = config.packages.iter().filter(|p| {
                cli.filter.as_ref().is_none_or(|re| {
                    let full_name = format!("{}/{}", p.repository.owner, p.repository.repo);
                    re.is_match(&full_name)
                })
            }).collect();
            if !packages.is_empty() {
                let start = random_range(0..packages.len());
                packages.rotate_left(start);
            }

            for package in packages {
                let repo_packages = &repo_packages;
                let repo_string = format!("{}/{}", package.repository.owner, package.repository.repo);

                let (repository, releases) =
                    match gh.query_releases(&package.repository, &package.name).await {
                        Ok((repository, releases)) => (repository, releases),
                        Err(_e) => {
                            result.push(package_generation::PackageResult {
                                repository: repo_string,
                                name: package.name.clone(),
                                versions: vec![VersionPackagingStatus {
                                    version: None,
                                    status: package_generation::PackagingStatus::github_failed(),
                                }],
                            });
                            continue;
                        }
                    };

                let (versions, generated_count) = package_generation::generate_packaging_data(
                    package,
                    &repository,
                    &releases,
                    repo_packages,
                    temporary_directory.path(),
                    PACKAGE_GENERATION_LIMIT - package_count,
                )?;
                package_count += generated_count;

                result.push(package_generation::PackageResult {
                    repository: repo_string,
                    name: package.name.clone(),
                    versions,
                });
                if package_count >= PACKAGE_GENERATION_LIMIT {
                    stop_reason = package_generation::StopReason::PackageLimit;
                    break;
                }
            }

            report_status(&temporary_directory, &result, &stop_reason)?;

            Ok(())
        })
}
