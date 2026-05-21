use anyhow::Result;
use kumo_core::Store;
use resolver::Resolver;
use security::SecurityEngine;
use std::collections::HashMap;

/// Represents an available upgrade for a single dependency.
struct UpgradeCandidate {
    name: String,
    current_version: String,
    latest_version: String,
    change_type: ChangeType,
    dep_section: DepSection,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChangeType {
    Major,
    Minor,
    Patch,
    UpToDate,
}

impl std::fmt::Display for ChangeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeType::Major => write!(f, "\x1b[31mmajor\x1b[0m"),
            ChangeType::Minor => write!(f, "\x1b[33mminor\x1b[0m"),
            ChangeType::Patch => write!(f, "\x1b[32mpatch\x1b[0m"),
            ChangeType::UpToDate => write!(f, "\x1b[90mup to date\x1b[0m"),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum DepSection {
    Dependencies,
    DevDependencies,
}

impl std::fmt::Display for DepSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DepSection::Dependencies => write!(f, "dep"),
            DepSection::DevDependencies => write!(f, "dev"),
        }
    }
}

pub async fn execute(
    store: &Store,
    resolver: &Resolver,
    security: &SecurityEngine,
    packages: Vec<String>,
    _latest: bool,
    prod_only: bool,
    dev_only: bool,
    fixed: bool,
    dry_run: bool,
    log: bool,
    config_path: std::path::PathBuf,
) -> Result<()> {
    // Read the configuration file
    let config_content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path)?)?;

    // Parse packages list to extract upgrade filter vs package names
    let mut upgrade_type = None;
    let mut target_packages = Vec::new();

    for pkg in packages {
        let pkg_lower = pkg.to_lowercase();
        if pkg_lower == "major" || pkg_lower == "minor" || pkg_lower == "patch" || pkg_lower == "parch" {
            upgrade_type = Some(match pkg_lower.as_str() {
                "major" => ChangeType::Major,
                "minor" => ChangeType::Minor,
                "patch" | "parch" => ChangeType::Patch,
                _ => unreachable!(),
            });
        } else {
            target_packages.push(pkg);
        }
    }

    // Collect dependencies to check based on --prod / --dev flags
    let mut deps_to_check: Vec<(String, String, DepSection)> = Vec::new();

    let include_prod = !dev_only;
    let include_dev = !prod_only;

    if include_prod {
        if let Some(d) = config_content.get("dependencies").and_then(|v| v.as_object()) {
            for (k, v) in d {
                deps_to_check.push((
                    k.clone(),
                    v.as_str().unwrap_or("latest").to_string(),
                    DepSection::Dependencies,
                ));
            }
        }
    }

    if include_dev {
        if let Some(d) = config_content.get("devDependencies").and_then(|v| v.as_object()) {
            for (k, v) in d {
                deps_to_check.push((
                    k.clone(),
                    v.as_str().unwrap_or("latest").to_string(),
                    DepSection::DevDependencies,
                ));
            }
        }
    }

    if deps_to_check.is_empty() {
        println!("No dependencies found to upgrade.");
        return Ok(());
    }

    // Filter to specific packages if provided
    if !target_packages.is_empty() {
        deps_to_check.retain(|(name, _, _)| target_packages.contains(name));
        if deps_to_check.is_empty() {
            println!("None of the specified packages were found in dependencies.");
            return Ok(());
        }
    }

    // Read current lockfile to get locked versions
    let lock_path = std::env::current_dir()?.join("kumo.lock");
    let locked_versions: HashMap<String, String> = if lock_path.exists() {
        let lockfile: resolver::Lockfile =
            serde_yaml::from_str(&std::fs::read_to_string(&lock_path)?)?;
        lockfile.dependencies
    } else {
        HashMap::new()
    };

    // Resolve target available versions for each dependency
    println!("Checking for updates...\n");

    let pb = indicatif::ProgressBar::new(deps_to_check.len() as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "{spinner:.cyan} [{bar:30.cyan/blue}] {pos}/{len} {msg}",
        )
        .unwrap()
        .progress_chars("━━╸━"),
    );
    pb.set_message("Fetching versions...");

    let mut candidates: Vec<UpgradeCandidate> = Vec::new();

    for (name, range, section) in &deps_to_check {
        pb.set_message(format!("{}", name));

        let current_version = locked_versions
            .get(name)
            .cloned()
            .unwrap_or_else(|| range.clone());

        let cur_version_opt = {
            let cleaned = current_version.trim_start_matches(|c: char| !c.is_ascii_digit());
            semver::Version::parse(cleaned).ok()
        };

        // Resolve the package metadata for the target upgrade version
        let meta = match upgrade_type {
            Some(ChangeType::Patch) | Some(ChangeType::Minor) => {
                if let Some(cur) = &cur_version_opt {
                    match resolver.get_available_versions(name).await {
                        Ok(versions) => {
                            let target_ver = versions.into_iter().filter(|v| {
                                match upgrade_type {
                                    Some(ChangeType::Patch) => {
                                        v.major == cur.major && v.minor == cur.minor && v > cur
                                    }
                                    Some(ChangeType::Minor) => {
                                        v.major == cur.major && v > cur
                                    }
                                    _ => false,
                                }
                            }).max();

                            if let Some(target) = target_ver {
                                match resolver.resolve_package_fresh(name, &target.to_string()).await {
                                    Ok(m) => m,
                                    Err(e) => {
                                        if log {
                                            eprintln!("  Warning: Could not resolve version {} for {}: {}", target, name, e);
                                        }
                                        pb.inc(1);
                                        continue;
                                    }
                                }
                            } else {
                                // No newer version matching the filter was found, treat as already up-to-date
                                match resolver.resolve_package_fresh(name, &current_version).await {
                                    Ok(m) => m,
                                    Err(_) => {
                                        match resolver.resolve_package_fresh(name, range).await {
                                            Ok(m) => m,
                                            Err(e) => {
                                                if log {
                                                    eprintln!("  Warning: Could not resolve {} (range {}): {}", name, range, e);
                                                }
                                                pb.inc(1);
                                                continue;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            if log {
                                eprintln!("  Warning: Could not get available versions for {}: {}", name, e);
                            }
                            pb.inc(1);
                            continue;
                        }
                    }
                } else {
                    // Current version cannot be parsed, fallback to latest
                    match resolver.resolve_package_fresh(name, "latest").await {
                        Ok(m) => m,
                        Err(e) => {
                            if log {
                                eprintln!("  Warning: Could not resolve latest version for {}: {}", name, e);
                            }
                            pb.inc(1);
                            continue;
                        }
                    }
                }
            }
            _ => {
                // By default (major, latest or None), upgrade to the absolute latest version
                match resolver.resolve_package_fresh(name, "latest").await {
                    Ok(m) => m,
                    Err(e) => {
                        if log {
                            eprintln!("  Warning: Could not resolve latest version for {}: {}", name, e);
                        }
                        pb.inc(1);
                        continue;
                    }
                }
            }
        };
        let new_version = meta.version.to_string();

        let has_scripts = meta.scripts.as_ref().map_or(false, |s| {
            s.contains_key("preinstall")
                || s.contains_key("install")
                || s.contains_key("postinstall")
        });

        match security
            .validate_package(
                name,
                &new_version,
                meta.license.as_deref(),
                meta.deprecated.is_some(),
                meta.published_at.as_deref(),
                has_scripts,
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("\n🚨 \x1b[31mSecurity Warning:\x1b[0m Upgrade candidate {}@{} violates security policy! Skipping this upgrade.", name, new_version);
                pb.inc(1);
                continue;
            }
            Err(e) => {
                if log {
                    eprintln!("  Warning: Could not validate security for {}@{}: {}", name, new_version, e);
                }
                pb.inc(1);
                continue;
            }
        }

        let change_type = classify_change(&current_version, &new_version);

        candidates.push(UpgradeCandidate {
            name: name.clone(),
            current_version,
            latest_version: new_version,
            change_type,
            dep_section: *section,
        });

        pb.inc(1);
    }

    pb.finish_and_clear();

    // Separate into upgradable and up-to-date
    let upgradable: Vec<&UpgradeCandidate> = candidates
        .iter()
        .filter(|c| !matches!(c.change_type, ChangeType::UpToDate))
        .collect();

    let up_to_date_count = candidates.len() - upgradable.len();

    if upgradable.is_empty() {
        println!(
            "\x1b[32m✓\x1b[0m All {} dependencies are already up to date!",
            candidates.len()
        );
        return Ok(());
    }

    // Print upgrade table
    print_upgrade_table(&upgradable, up_to_date_count);

    if dry_run {
        println!(
            "\n\x1b[33m⚠\x1b[0m Dry run: no changes applied. Remove --dry-run to upgrade."
        );
        return Ok(());
    }

    // Apply upgrades: update config file
    let mut config_content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path)?)?;

    for candidate in &upgradable {
        let section = match candidate.dep_section {
            DepSection::Dependencies => "dependencies",
            DepSection::DevDependencies => "devDependencies",
        };

        if let Some(deps) = config_content
            .get_mut(section)
            .and_then(|v| v.as_object_mut())
        {
            if deps.contains_key(&candidate.name) {
                let new_range = if fixed {
                    candidate.latest_version.clone()
                } else {
                    let original = deps
                        .get(&candidate.name)
                        .and_then(|v| v.as_str())
                        .unwrap_or("latest");
                    let trimmed = original.trim();
                    let prefix_end = trimmed
                        .find(|c: char| c.is_ascii_digit())
                        .unwrap_or(0);
                    let prefix = &trimmed[..prefix_end];
                    if prefix.is_empty() {
                        candidate.latest_version.clone()
                    } else {
                        update_range_version(original, &candidate.latest_version)
                    }
                };
                deps.insert(
                    candidate.name.clone(),
                    serde_json::Value::String(new_range),
                );
            }
        }
    }

    let json = serde_json::to_string_pretty(&config_content)?;
    std::fs::write(&config_path, &json)?;

    println!(
        "\n\x1b[32m✓\x1b[0m Updated {} in {}",
        upgradable.len(),
        config_path.file_name().unwrap().to_string_lossy()
    );


    // Re-resolve and reinstall by collecting all deps from the updated config
    println!("Resolving and installing updated dependencies...\n");

    let updated_config: serde_json::Value = serde_json::from_str(&json)?;
    let mut all_deps = HashMap::new();

    if let Some(d) = updated_config
        .get("dependencies")
        .and_then(|v| v.as_object())
    {
        for (k, v) in d {
            all_deps.insert(k.clone(), v.as_str().unwrap_or("latest").to_string());
        }
    }
    if let Some(d) = updated_config
        .get("devDependencies")
        .and_then(|v| v.as_object())
    {
        for (k, v) in d {
            all_deps.insert(k.clone(), v.as_str().unwrap_or("latest").to_string());
        }
    }

    // Delete the lockfile config_hash to force a full re-resolution
    if lock_path.exists() {
        let lock_content = std::fs::read_to_string(&lock_path)?;
        if let Ok(mut lockfile) = serde_yaml::from_str::<resolver::Lockfile>(&lock_content) {
            lockfile.config_hash = None;
            let yaml = serde_yaml::to_string(&lockfile)?;
            std::fs::write(&lock_path, yaml)?;
        }
    }

    crate::commands::install::resolve_and_install(store, resolver, security, all_deps, log, config_path)
        .await?;

    println!(
        "\n\x1b[32m✓\x1b[0m Successfully upgraded {} packages!",
        upgradable.len()
    );

    Ok(())
}

/// Classifies the type of version change between current and new.
fn classify_change(current: &str, new: &str) -> ChangeType {
    let cur = match semver::Version::parse(current) {
        Ok(v) => v,
        Err(_) => return if current == new { ChangeType::UpToDate } else { ChangeType::Major },
    };
    let latest = match semver::Version::parse(new) {
        Ok(v) => v,
        Err(_) => return ChangeType::Major,
    };

    if cur == latest {
        ChangeType::UpToDate
    } else if latest.major != cur.major {
        ChangeType::Major
    } else if latest.minor != cur.minor {
        ChangeType::Minor
    } else {
        ChangeType::Patch
    }
}

/// Updates the version in a semver range string while preserving the range prefix.
/// e.g., "^1.2.3" + "1.5.0" -> "^1.5.0", "~2.0.0" + "2.1.0" -> "~2.1.0"
fn update_range_version(original_range: &str, new_version: &str) -> String {
    let trimmed = original_range.trim();
    if trimmed == "latest" || trimmed == "*" || trimmed.is_empty() {
        return format!("^{}", new_version);
    }

    // Extract the prefix (^, ~, >=, etc.)
    let prefix_end = trimmed
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(0);
    let prefix = &trimmed[..prefix_end];

    if prefix.is_empty() {
        // Exact version, keep it exact
        new_version.to_string()
    } else {
        format!("{}{}", prefix, new_version)
    }
}

/// Prints a formatted table of available upgrades.
fn print_upgrade_table(upgradable: &[&UpgradeCandidate], up_to_date_count: usize) {
    // Calculate column widths
    let name_width = upgradable
        .iter()
        .map(|c| c.name.len())
        .max()
        .unwrap_or(7)
        .max(7);
    let cur_width = upgradable
        .iter()
        .map(|c| c.current_version.len())
        .max()
        .unwrap_or(7)
        .max(7);
    let new_width = upgradable
        .iter()
        .map(|c| c.latest_version.len())
        .max()
        .unwrap_or(6)
        .max(6);

    println!(
        "\x1b[1m  {:<nw$}  {:<cw$}  {:<lw$}  {:<6}  {:<4}\x1b[0m",
        "Package",
        "Current",
        "Latest",
        "Change",
        "Type",
        nw = name_width,
        cw = cur_width,
        lw = new_width,
    );
    println!(
        "  {}\x1b[0m",
        "─".repeat(name_width + cur_width + new_width + 24)
    );

    for c in upgradable {
        println!(
            "  {:<nw$}  \x1b[90m{:<cw$}\x1b[0m  \x1b[32m{:<lw$}\x1b[0m  {:<16}  \x1b[90m{}\x1b[0m",
            c.name,
            c.current_version,
            c.latest_version,
            c.change_type,
            c.dep_section,
            nw = name_width,
            cw = cur_width,
            lw = new_width,
        );
    }

    println!(
        "\n  \x1b[36m{}\x1b[0m package(s) to upgrade, \x1b[90m{}\x1b[0m already up to date.",
        upgradable.len(),
        up_to_date_count,
    );
}
