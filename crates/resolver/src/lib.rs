use anyhow::{anyhow, Context, Result};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PackageMetadata {
    pub name: String,
    pub version: Version,
    pub dependencies: Option<HashMap<String, String>>,
    pub dist: TarballInfo,
    pub license: Option<String>,
    pub deprecated: Option<String>,
    pub published_at: Option<String>,
    pub bin: Option<serde_json::Value>,
    pub scripts: Option<HashMap<String, String>>,
    pub optional_dependencies: Option<HashMap<String, String>>,
    pub os: Option<Vec<String>>,
    pub cpu: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct RegistrySignature {
    pub keyid: String,
    pub sig: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct RegistryAttestations {
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TarballInfo {
    pub tarball: String,
    pub shasum: String,
    #[serde(default)]
    pub size: u64,
    #[serde(rename = "unpackedSize", default)]
    pub unpacked_size: u64,
    #[serde(default)]
    pub signatures: Option<Vec<RegistrySignature>>,
    #[serde(rename = "npm-signature", default)]
    pub npm_signature: Option<String>,
    #[serde(default)]
    pub attestations: Option<RegistryAttestations>,
}

impl TarballInfo {
    pub fn get_size(&self) -> u64 {
        if self.unpacked_size > 0 {
            self.unpacked_size
        } else {
            self.size
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct RegistryResponse {
    name: String,
    versions: HashMap<String, RegistryVersion>,
    #[serde(rename = "dist-tags")]
    dist_tags: HashMap<String, String>,
    time: Option<HashMap<String, String>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RegistryVersion {
    name: String,
    version: String,
    dependencies: Option<HashMap<String, String>>,
    dist: TarballInfo,
    license: Option<serde_json::Value>,
    deprecated: Option<serde_json::Value>,
    scripts: Option<HashMap<String, String>>,
    bin: Option<serde_json::Value>,
    #[serde(rename = "optionalDependencies")]
    pub optional_dependencies: Option<HashMap<String, String>>,
    pub os: Option<Vec<String>>,
    pub cpu: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Lockfile {
    pub lockfile_version: String,
    pub config_hash: Option<String>,
    pub dependencies: HashMap<String, String>,
    pub packages: HashMap<String, LockedPackage>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LockedPackage {
    pub resolution: TarballInfo,
    pub dependencies: Option<HashMap<String, String>>,
    pub bin: Option<serde_json::Value>,
    pub scripts: Option<HashMap<String, String>>,
    pub optional_dependencies: Option<HashMap<String, String>>,
    #[serde(default)]
    pub published_at: Option<String>,
}

#[derive(Clone)]
pub struct Resolver {
    client: reqwest::Client,
    registry_url: String,
}

impl Resolver {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .http2_prior_knowledge()
            .pool_max_idle_per_host(20)
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_nodelay(true)
            .user_agent(format!("kumo-pkg/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            registry_url: "https://registry.npmjs.org".to_string(),
        }
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub async fn resolve_package(self, name: String, range: String) -> Result<PackageMetadata> {
        self.resolve_package_internal(&name, &range).await
    }

    /// Resolves a package by invalidating the metadata cache first (fresh fetch).
    pub async fn resolve_package_fresh(&self, name: &str, range: &str) -> Result<PackageMetadata> {
        let cache_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".kumo")
            .join("cache")
            .join("metadata");
        let cache_path = cache_dir.join(format!("{}.json", name.replace('/', "__")));
        let _ = std::fs::remove_file(&cache_path);

        self.resolve_package_internal(name, range).await
    }

    /// Gets the absolute latest version of a package from the registry (dist-tags.latest).
    pub async fn get_latest_version(&self, name: &str) -> Result<String> {
        let cache_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".kumo")
            .join("cache")
            .join("metadata");
        let cache_path = cache_dir.join(format!("{}.json", name.replace('/', "__")));
        let _ = std::fs::remove_file(&cache_path);

        let response = self.fetch_and_cache_metadata(name, &cache_path).await?;
        response
            .dist_tags
            .get("latest")
            .cloned()
            .ok_or_else(|| anyhow!("No latest tag found for {}", name))
    }

    /// Gets all available versions of a package from the registry metadata cache or registry.
    pub async fn get_available_versions(&self, name: &str) -> Result<Vec<Version>> {
        let cache_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".kumo")
            .join("cache")
            .join("metadata");
        let cache_path = cache_dir.join(format!("{}.json", name.replace('/', "__")));

        let response: RegistryResponse = if cache_path.exists() {
            let content = std::fs::read_to_string(&cache_path)?;
            if let Ok(res) = serde_json::from_str::<RegistryResponse>(&content) {
                if res.versions.is_empty() {
                    self.fetch_and_cache_metadata(name, &cache_path).await?
                } else {
                    res
                }
            } else {
                self.fetch_and_cache_metadata(name, &cache_path).await?
            }
        } else {
            self.fetch_and_cache_metadata(name, &cache_path).await?
        };

        let mut versions: Vec<Version> = response
            .versions
            .keys()
            .filter_map(|v| Version::parse(v).ok())
            .collect();
        versions.sort();
        Ok(versions)
    }

    async fn resolve_package_internal(&self, name: &str, range: &str) -> Result<PackageMetadata> {
        let cache_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".kumo")
            .join("cache")
            .join("metadata");

        let _ = std::fs::create_dir_all(&cache_dir);
        let cache_path = cache_dir.join(format!("{}.json", name.replace('/', "__")));

        let response: RegistryResponse = if cache_path.exists() {
            let content = std::fs::read_to_string(&cache_path)?;
            let res: RegistryResponse = serde_json::from_str(&content)?;
            if res.versions.is_empty() {
                self.fetch_and_cache_metadata(name, &cache_path).await?
            } else {
                res
            }
        } else {
            self.fetch_and_cache_metadata(name, &cache_path).await?
        };

        let version_str = if response.dist_tags.contains_key(range) {
            response
                .dist_tags
                .get(range)
                .unwrap()
                .to_string()
        } else if range == "latest" || range == "*" || range == "" {
            response
                .dist_tags
                .get("latest")
                .ok_or_else(|| anyhow!("No latest tag found for {}", name))?
                .to_string()
        } else {
            let reqs: Vec<VersionReq> = range
                .split("||")
                .map(|r| {
                    let normalized = r
                        .trim()
                        .replace(">= ", ">=")
                        .replace("<= ", "<=")
                        .replace("> ", ">")
                        .replace("< ", "<")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(",");
                    VersionReq::parse(&normalized)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| anyhow!("Invalid semver range: {}", range))?;

            let mut versions: Vec<Version> = response
                .versions
                .keys()
                .filter_map(|v| Version::parse(v).ok())
                .filter(|v| reqs.iter().any(|req| req.matches(v)))
                .collect();

            versions.sort();
            versions
                .last()
                .map(|v| v.to_string())
                .ok_or_else(|| anyhow!("No version matching {} found for {}", range, name))?
        };

        let version_data = response
            .versions
            .get(&version_str)
            .ok_or_else(|| anyhow!("Version data for {} not found", version_str))?;

        let published_at = response
            .time
            .as_ref()
            .and_then(|t| t.get(&version_str))
            .cloned();

        let license = version_data.license.as_ref().and_then(|l| {
            if let Some(s) = l.as_str() {
                Some(s.to_string())
            } else {
                l.get("type")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string())
            }
        });

        let deprecated = version_data.deprecated.as_ref().and_then(|d| {
            if let Some(s) = d.as_str() {
                Some(s.to_string())
            } else {
                None
            }
        });

        Ok(PackageMetadata {
            name: version_data.name.clone(),
            version: Version::parse(&version_data.version)?,
            dependencies: version_data.dependencies.clone(),
            dist: version_data.dist.clone(),
            license,
            deprecated,
            published_at,
            bin: version_data.bin.clone(),
            scripts: version_data.scripts.clone(),
            optional_dependencies: version_data.optional_dependencies.clone(),
            os: version_data.os.clone(),
            cpu: version_data.cpu.clone(),
        })
    }

    fn is_compatible(os_list: &Option<Vec<String>>, cpu_list: &Option<Vec<String>>) -> bool {
        let current_os = match std::env::consts::OS {
            "windows" => "win32",
            "macos" => "darwin",
            os => os,
        };
        let current_arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "x86" => "ia32",
            "aarch64" => "arm64",
            "powerpc" => "ppc",
            "powerpc64" => "ppc64",
            arch => arch,
        };

        if let Some(os) = os_list {
            if !os.is_empty() {
                let mut match_found = false;
                let mut has_negation = false;
                for o in os {
                    if o.starts_with('!') {
                        has_negation = true;
                        if &o[1..] == current_os {
                            return false;
                        }
                    } else if o == current_os {
                        match_found = true;
                    }
                }
                if !match_found && !has_negation {
                    return false;
                }
            }
        }

        if let Some(cpu) = cpu_list {
            if !cpu.is_empty() {
                let mut match_found = false;
                let mut has_negation = false;
                for c in cpu {
                    if c.starts_with('!') {
                        has_negation = true;
                        if &c[1..] == current_arch {
                            return false;
                        }
                    } else if c == current_arch {
                        match_found = true;
                    }
                }
                if !match_found && !has_negation {
                    return false;
                }
            }
        }

        true
    }

    pub async fn resolve_tree(&self, root_deps: &HashMap<String, String>) -> Result<Lockfile> {
        use futures::future::BoxFuture;
        use futures::stream::{FuturesUnordered, StreamExt};
        use std::sync::{Arc, Mutex};

        let packages = Arc::new(Mutex::new(HashMap::new()));
        let resolved_root_deps = Arc::new(Mutex::new(HashMap::new()));
        let queue = Arc::new(Mutex::new(
            root_deps
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>(),
        ));

        let mut cache_empty = true;
        let cache_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".kumo")
            .join("cache")
            .join("metadata");
        if cache_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(cache_dir) {
                if entries.count() > 10 {
                    cache_empty = false;
                }
            }
        }

        if cache_empty {
            println!("Note: Metadata cache is empty or small. Resolution may take longer as we fetch from registry.");
        }

        let resolver = Arc::new(self.clone());
        let mut active_resolutions: FuturesUnordered<
            BoxFuture<'static, (String, Result<PackageMetadata>)>,
        > = FuturesUnordered::new();

        let mut initial_queue = queue.lock().unwrap();
        while let Some(item) = initial_queue.pop() {
            let r = (*resolver).clone();
            active_resolutions.push(Box::pin(async move {
                let res = r.resolve_package(item.0.clone(), item.1.clone()).await;
                (item.0, res)
            }));
        }
        drop(initial_queue);

        while let Some((req_name, metadata_res)) = active_resolutions.next().await {
            let metadata = metadata_res?;

            if !Self::is_compatible(&metadata.os, &metadata.cpu) {
                continue;
            }

            let key = format!("{}@{}", metadata.name, metadata.version);

            let mut pkgs = packages.lock().unwrap();
            if !pkgs.contains_key(&key) {
                let mut all_deps = metadata.dependencies.clone().unwrap_or_default();
                if let Some(opt_deps) = &metadata.optional_dependencies {
                    for (k, v) in opt_deps {
                        all_deps.insert(k.clone(), v.clone());
                    }
                }

                pkgs.insert(
                    key.clone(),
                    LockedPackage {
                        resolution: metadata.dist.clone(),
                        dependencies: metadata.dependencies.clone(),
                        bin: metadata.bin.clone(),
                        scripts: metadata.scripts.clone(),
                        optional_dependencies: metadata.optional_dependencies.clone(),
                        published_at: metadata.published_at.clone(),
                    },
                );
                drop(pkgs);

                for (d_name, d_range) in all_deps {
                    let r = (*resolver).clone();
                    active_resolutions.push(Box::pin(async move {
                        let res = r.resolve_package(d_name.clone(), d_range.clone()).await;
                        (d_name, res)
                    }));
                }
            } else {
                drop(pkgs);
            }

            if root_deps.contains_key(&req_name) {
                resolved_root_deps
                    .lock()
                    .unwrap()
                    .insert(req_name, metadata.version.to_string());
            }
        }

        let final_packages = Arc::try_unwrap(packages).unwrap().into_inner().unwrap();
        let final_root_deps = Arc::try_unwrap(resolved_root_deps)
            .unwrap()
            .into_inner()
            .unwrap();

        Ok(Lockfile {
            lockfile_version: "1.0".to_string(),
            config_hash: None,
            dependencies: final_root_deps,
            packages: final_packages,
        })
    }

    async fn fetch_and_cache_metadata(
        &self,
        name: &str,
        cache_path: &std::path::Path,
    ) -> Result<RegistryResponse> {
        let url = format!("{}/{}", self.registry_url, name);
        let mut last_err = None;

        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(500 * attempt)).await;
            }

            match self.client.get(&url).send().await {
                Ok(res) => {
                    let metadata: RegistryResponse = res
                        .json()
                        .await
                        .with_context(|| format!("Failed to parse metadata for {}", name))?;
                    let json = serde_json::to_string(&metadata)?;
                    let _ = std::fs::write(cache_path, json);
                    return Ok(metadata);
                }
                Err(e) => {
                    last_err = Some(e);
                }
            }
        }

        Err(anyhow!(
            "Failed to fetch metadata for {} after 3 attempts: {:?}",
            name,
            last_err
        ))
    }
}
