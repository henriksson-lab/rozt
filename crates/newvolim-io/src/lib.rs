//! OME-NGFF metadata and coordinate-transform primitives.
//!
//! Transform composition is deliberately independent of stores and rendering. A level's array
//! coordinates can therefore be mapped into a named physical coordinate system before a camera,
//! picker, or renderer sees them.

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

use opendal::{services, Operator};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DatasetMetadata {
    #[serde(default)]
    pub multiscales: Vec<Multiscale>,
    #[serde(default)]
    pub omero: Option<OmeroMetadata>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Multiscale {
    pub axes: Vec<Axis>,
    pub datasets: Vec<MultiscaleDataset>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "coordinateTransformations")]
    pub coordinate_transformations: Vec<CoordinateTransformation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Axis {
    pub name: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiscaleDataset {
    pub path: String,
    #[serde(default, rename = "coordinateTransformations")]
    pub coordinate_transformations: Vec<CoordinateTransformation>,
}

/// Legacy NGFF coordinate-transform entries. They are also the operations used by named
/// coordinate-system graph edges below.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CoordinateTransformation {
    #[serde(rename = "scale")]
    Scale { scale: Vec<f64> },
    #[serde(rename = "translation")]
    Translation { translation: Vec<f64> },
    #[serde(rename = "affine")]
    Affine { matrix: Vec<Vec<f64>> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OmeroMetadata {
    #[serde(default)]
    pub channels: Vec<OmeroChannel>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OmeroChannel {
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub window: Option<OmeroWindow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OmeroWindow {
    pub start: f64,
    pub end: f64,
    pub min: f64,
    pub max: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArrayInfo {
    pub shape: Vec<u64>,
    pub chunks: Vec<u64>,
    pub dtype: String,
    #[serde(default)]
    pub order: Option<String>,
    #[serde(default)]
    pub compressor: Option<serde_json::Value>,
}

/// Largest accepted root metadata document. Metadata is untrusted input when a store is remote;
/// this bound prevents a malformed `zarr.json` from becoming an unbounded allocation before the
/// source permission layer has even opened an array.
pub const MAX_ROOT_METADATA_BYTES: u64 = 16 * 1024 * 1024;

/// Default cap for one compressed remote Zarr asset. This is intentionally independent of the
/// metadata cap: a valid chunk can be larger than metadata, but neither may allocate without a
/// caller-visible budget.
pub const MAX_REMOTE_ASSET_BYTES: u64 = 64 * 1024 * 1024;

/// Local filesystem capability used by desktop and server callers before opening a dataset.
///
/// Paths are canonicalized before comparison, so a symlink under an allowed root cannot escape
/// that root. An empty policy grants no filesystem access; callers must opt in to each root.
#[derive(Clone, Debug, Default)]
pub struct LocalSourcePolicy {
    roots: Vec<PathBuf>,
}

impl LocalSourcePolicy {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Result<Self, SourcePolicyError> {
        let mut canonical_roots = Vec::new();
        for root in roots {
            let root = root.canonicalize().map_err(SourcePolicyError::Io)?;
            if !root.is_dir() {
                return Err(SourcePolicyError::NotDirectory(root));
            }
            if !canonical_roots.contains(&root) {
                canonical_roots.push(root);
            }
        }
        Ok(Self {
            roots: canonical_roots,
        })
    }

    pub fn authorize(&self, candidate: impl AsRef<Path>) -> Result<PathBuf, SourcePolicyError> {
        let candidate = candidate
            .as_ref()
            .canonicalize()
            .map_err(SourcePolicyError::Io)?;
        if !candidate.is_dir() {
            return Err(SourcePolicyError::NotDirectory(candidate));
        }
        if self.roots.iter().any(|root| candidate.starts_with(root)) {
            Ok(candidate)
        } else {
            Err(SourcePolicyError::NotAllowed(candidate))
        }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
}

#[derive(Debug)]
pub enum SourcePolicyError {
    Io(std::io::Error),
    NotAllowed(PathBuf),
    NotDirectory(PathBuf),
}

impl fmt::Display for SourcePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "local source filesystem error: {error}"),
            Self::NotAllowed(path) => write!(
                formatter,
                "local source is outside the configured allow-list: {}",
                path.display()
            ),
            Self::NotDirectory(path) => {
                write!(
                    formatter,
                    "local source is not a directory: {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for SourcePolicyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::NotAllowed(_) | Self::NotDirectory(_) => None,
        }
    }
}

/// Explicit HTTPS host allow-list for future HTTP/S3-backed Zarr stores. It authorizes an origin,
/// never follows a redirect implicitly; the fetch layer must re-authorize every redirect target.
#[derive(Clone, Debug, Default)]
pub struct RemoteSourcePolicy {
    hosts: Vec<String>,
}

impl RemoteSourcePolicy {
    pub fn new(hosts: impl IntoIterator<Item = String>) -> Result<Self, RemoteSourcePolicyError> {
        let mut allowed = Vec::new();
        for host in hosts {
            let host = host.trim().to_ascii_lowercase();
            if host.is_empty() || host.contains('/') || host.contains(':') {
                return Err(RemoteSourcePolicyError::InvalidHost(host));
            }
            if !allowed.contains(&host) {
                allowed.push(host);
            }
        }
        Ok(Self { hosts: allowed })
    }

    pub fn authorize(&self, candidate: &str) -> Result<Url, RemoteSourcePolicyError> {
        let url = Url::parse(candidate).map_err(RemoteSourcePolicyError::Url)?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            return Err(RemoteSourcePolicyError::UnsafeUrl(url));
        }
        if url.port_or_known_default() != Some(443) {
            return Err(RemoteSourcePolicyError::UnsafeUrl(url));
        }
        let host = url
            .host_str()
            .ok_or_else(|| RemoteSourcePolicyError::UnsafeUrl(url.clone()))?;
        if !self.hosts.iter().any(|allowed| allowed == host) {
            return Err(RemoteSourcePolicyError::NotAllowed(url));
        }
        Ok(url)
    }
}

#[derive(Debug)]
pub enum RemoteSourcePolicyError {
    InvalidHost(String),
    NotAllowed(Url),
    UnsafeUrl(Url),
    Url(url::ParseError),
}

impl fmt::Display for RemoteSourcePolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHost(host) => write!(f, "invalid allowed remote host {host:?}"),
            Self::NotAllowed(url) => write!(f, "remote source is not allow-listed: {url}"),
            Self::UnsafeUrl(url) => write!(f, "remote source must be credential-free HTTPS: {url}"),
            Self::Url(error) => write!(f, "invalid remote source URL: {error}"),
        }
    }
}

impl std::error::Error for RemoteSourcePolicyError {}

/// Fetch a small remote metadata document. Redirects are intentionally handled here, rather
/// than by reqwest, so every destination passes through [`RemoteSourcePolicy::authorize`].
pub fn fetch_remote_metadata(
    policy: &RemoteSourcePolicy,
    requested: &str,
) -> Result<Vec<u8>, RemoteFetchError> {
    fetch_remote_bytes(policy, requested, MAX_ROOT_METADATA_BYTES)
}

/// Fetch an authorized remote asset under an explicit byte budget. Redirects are disabled in
/// reqwest and re-authorized one at a time, so a permitted HTTPS origin cannot turn this helper
/// into an SSRF redirector.
pub fn fetch_remote_bytes(
    policy: &RemoteSourcePolicy,
    requested: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, RemoteFetchError> {
    const MAX_REDIRECTS: usize = 3;
    if max_bytes == 0 {
        return Err(RemoteFetchError::InvalidByteLimit);
    }
    let read_limit = max_bytes
        .checked_add(1)
        .ok_or(RemoteFetchError::InvalidByteLimit)?;
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(RemoteFetchError::Http)?;
    let mut url = policy
        .authorize(requested)
        .map_err(RemoteFetchError::Policy)?;
    for _ in 0..=MAX_REDIRECTS {
        let response = client
            .get(url.clone())
            .send()
            .map_err(RemoteFetchError::Http)?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or(RemoteFetchError::MissingRedirectLocation)?
                .to_str()
                .map_err(|_| RemoteFetchError::InvalidRedirectLocation)?;
            let next = url.join(location).map_err(RemoteFetchError::Url)?;
            url = policy
                .authorize(next.as_str())
                .map_err(RemoteFetchError::Policy)?;
            continue;
        }
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(RemoteFetchError::NotFound);
        }
        let response = response
            .error_for_status()
            .map_err(RemoteFetchError::Http)?;
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes)
        {
            return Err(RemoteFetchError::TooLarge);
        }
        let mut response = response;
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(read_limit)
            .read_to_end(&mut bytes)
            .map_err(RemoteFetchError::Read)?;
        if bytes.len() as u64 > max_bytes {
            return Err(RemoteFetchError::TooLarge);
        }
        return Ok(bytes);
    }
    Err(RemoteFetchError::TooManyRedirects)
}

#[derive(Debug)]
pub enum RemoteFetchError {
    Http(reqwest::Error),
    InvalidByteLimit,
    Read(std::io::Error),
    InvalidRedirectLocation,
    MissingRedirectLocation,
    Policy(RemoteSourcePolicyError),
    NotFound,
    TooLarge,
    TooManyRedirects,
    Url(url::ParseError),
}

impl fmt::Display for RemoteFetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(error) => write!(f, "remote asset request failed: {error}"),
            Self::InvalidByteLimit => write!(f, "remote asset byte limit must be non-zero"),
            Self::Read(error) => write!(f, "remote asset response read failed: {error}"),
            Self::InvalidRedirectLocation => write!(f, "remote redirect has a non-UTF-8 location"),
            Self::MissingRedirectLocation => write!(f, "remote redirect lacks a location"),
            Self::Policy(error) => write!(f, "remote redirect/source rejected: {error}"),
            Self::NotFound => write!(f, "remote asset was not found"),
            Self::TooLarge => write!(f, "remote asset exceeds the configured size limit"),
            Self::TooManyRedirects => write!(f, "remote source exceeded redirect limit"),
            Self::Url(error) => write!(f, "invalid remote redirect URL: {error}"),
        }
    }
}

impl std::error::Error for RemoteFetchError {}

/// Reads OME-NGFF group attributes from a remote Zarr root. The root URL must already be
/// authorized by `policy`; individual metadata URLs and every redirect are authorized again by
/// [`fetch_remote_metadata`]. A root without a trailing slash is treated as a directory, rather
/// than allowing URL resolution to replace its last path segment.
pub fn read_remote_dataset_metadata(
    policy: &RemoteSourcePolicy,
    root: &str,
) -> Result<DatasetMetadata, RemoteMetadataError> {
    let root = remote_root_url(policy, root)?;
    let v3 = root.join("zarr.json").map_err(RemoteMetadataError::Url)?;
    match fetch_remote_metadata(policy, v3.as_str()) {
        Ok(bytes) => {
            let document: ZarrV3Root =
                serde_json::from_slice(&bytes).map_err(RemoteMetadataError::Json)?;
            Ok(document.attributes)
        }
        Err(RemoteFetchError::NotFound) => {
            let v2 = root.join(".zattrs").map_err(RemoteMetadataError::Url)?;
            let bytes =
                fetch_remote_metadata(policy, v2.as_str()).map_err(RemoteMetadataError::Fetch)?;
            serde_json::from_slice(&bytes).map_err(RemoteMetadataError::Json)
        }
        Err(error) => Err(RemoteMetadataError::Fetch(error)),
    }
}

fn remote_root_url(policy: &RemoteSourcePolicy, root: &str) -> Result<Url, RemoteMetadataError> {
    let mut root = policy
        .authorize(root)
        .map_err(RemoteMetadataError::Policy)?;
    if root.query().is_some() || root.fragment().is_some() {
        return Err(RemoteMetadataError::InvalidRootUrl(root));
    }
    if !root.path().ends_with('/') {
        let path = format!("{}/", root.path());
        root.set_path(&path);
    }
    Ok(root)
}

/// A capability-scoped HTTPS OME-Zarr root that can derive and fetch bounded chunk assets.
///
/// It intentionally does not accept credentials, query strings, absolute asset paths, or path
/// traversal. S3 profiles and SSH-agent credentials remain separate source implementations;
/// this is the safe generic HTTPS leg shared by metadata and chunk reads.
#[derive(Clone, Debug)]
pub struct RemoteZarrStore {
    policy: RemoteSourcePolicy,
    root: Url,
    max_asset_bytes: u64,
}

impl RemoteZarrStore {
    pub fn new(
        policy: RemoteSourcePolicy,
        root: &str,
        max_asset_bytes: u64,
    ) -> Result<Self, RemoteStoreError> {
        if max_asset_bytes == 0 {
            return Err(RemoteStoreError::InvalidByteLimit);
        }
        let root = remote_root_url(&policy, root).map_err(RemoteStoreError::Root)?;
        Ok(Self {
            policy,
            root,
            max_asset_bytes,
        })
    }

    pub fn root(&self) -> &Url {
        &self.root
    }

    pub fn asset_url(&self, asset: &str) -> Result<Url, RemoteStoreError> {
        if !is_normal_asset_path(asset) {
            return Err(RemoteStoreError::InvalidAssetPath(asset.to_owned()));
        }
        self.root.join(asset).map_err(RemoteStoreError::Url)
    }

    pub fn fetch_asset(&self, asset: &str) -> Result<Vec<u8>, RemoteStoreError> {
        let url = self.asset_url(asset)?;
        fetch_remote_bytes(&self.policy, url.as_str(), self.max_asset_bytes)
            .map_err(RemoteStoreError::Fetch)
    }

    /// Reads root OME-NGFF attributes through this store's bounded, capability-scoped fetch
    /// path. As for local roots, Zarr v3 is preferred and legacy Zarr v2 is a fallback only when
    /// the v3 root document is absent.
    pub fn read_dataset_metadata(&self) -> Result<DatasetMetadata, RemoteStoreError> {
        match self.fetch_asset("zarr.json") {
            Ok(bytes) => parse_v3_root_metadata(&bytes).map_err(RemoteStoreError::Json),
            Err(RemoteStoreError::Fetch(RemoteFetchError::NotFound)) => {
                let bytes = self.fetch_asset(".zattrs")?;
                serde_json::from_slice(&bytes).map_err(RemoteStoreError::Json)
            }
            Err(error) => Err(error),
        }
    }

    /// Reads compact metadata for one normal relative array path. The path is derived through
    /// [`Self::asset_url`], so it cannot escape this store's authorized root.
    pub fn read_array_info(&self, relative_path: &str) -> Result<ArrayInfo, RemoteStoreError> {
        let v3 = format!("{relative_path}/zarr.json");
        match self.fetch_asset(&v3) {
            Ok(bytes) => parse_v3_array_info(&bytes).map_err(RemoteStoreError::Json),
            Err(RemoteStoreError::Fetch(RemoteFetchError::NotFound)) => {
                let bytes = self.fetch_asset(&format!("{relative_path}/.zarray"))?;
                serde_json::from_slice(&bytes).map_err(RemoteStoreError::Json)
            }
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug)]
pub enum RemoteStoreError {
    Fetch(RemoteFetchError),
    InvalidAssetPath(String),
    InvalidByteLimit,
    Json(serde_json::Error),
    Root(RemoteMetadataError),
    Url(url::ParseError),
}

impl fmt::Display for RemoteStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(error) => write!(formatter, "remote Zarr asset fetch failed: {error}"),
            Self::InvalidAssetPath(path) => write!(
                formatter,
                "remote Zarr asset path must be a non-empty normal relative path: {path:?}"
            ),
            Self::InvalidByteLimit => {
                write!(formatter, "remote Zarr asset byte limit must be non-zero")
            }
            Self::Json(error) => write!(formatter, "invalid remote OME-Zarr metadata: {error}"),
            Self::Root(error) => write!(formatter, "remote Zarr root rejected: {error}"),
            Self::Url(error) => write!(formatter, "invalid remote Zarr asset URL: {error}"),
        }
    }
}

impl std::error::Error for RemoteStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Fetch(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Root(error) => Some(error),
            Self::Url(error) => Some(error),
            Self::InvalidAssetPath(_) | Self::InvalidByteLimit => None,
        }
    }
}

/// Explicit S3 bucket and credential-profile policy. A caller must select both a bucket and a
/// named profile that this policy admits; no implicit default profile is used by this boundary.
#[derive(Clone, Debug, Default)]
pub struct S3SourcePolicy {
    buckets: Vec<String>,
    profiles: Vec<String>,
}

impl S3SourcePolicy {
    pub fn new(
        buckets: impl IntoIterator<Item = String>,
        profiles: impl IntoIterator<Item = String>,
    ) -> Result<Self, S3SourcePolicyError> {
        let mut allowed_buckets = Vec::new();
        for bucket in buckets {
            let bucket = bucket.trim().to_owned();
            if !is_valid_s3_bucket(&bucket) {
                return Err(S3SourcePolicyError::InvalidBucket(bucket));
            }
            if !allowed_buckets.contains(&bucket) {
                allowed_buckets.push(bucket);
            }
        }
        let mut allowed_profiles = Vec::new();
        for profile in profiles {
            let profile = profile.trim().to_owned();
            if !is_valid_profile_name(&profile) {
                return Err(S3SourcePolicyError::InvalidProfile(profile));
            }
            if !allowed_profiles.contains(&profile) {
                allowed_profiles.push(profile);
            }
        }
        Ok(Self {
            buckets: allowed_buckets,
            profiles: allowed_profiles,
        })
    }

    fn authorize(&self, bucket: &str, profile: &str) -> Result<(), S3SourcePolicyError> {
        if !self.buckets.iter().any(|allowed| allowed == bucket) {
            return Err(S3SourcePolicyError::BucketNotAllowed(bucket.to_owned()));
        }
        if !self.profiles.iter().any(|allowed| allowed == profile) {
            return Err(S3SourcePolicyError::ProfileNotAllowed(profile.to_owned()));
        }
        Ok(())
    }
}

fn is_valid_s3_bucket(value: &str) -> bool {
    (3..=63).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
        // Dotted decimal strings are not valid virtual-hosted S3 bucket names. Rejecting them
        // also keeps an administrator's allow-list from accidentally looking like an IP target.
        && value.parse::<std::net::Ipv4Addr>().is_err()
        && value.split('.').all(|label| {
            !label.is_empty()
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

fn is_valid_profile_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[derive(Debug, Eq, PartialEq)]
pub enum S3SourcePolicyError {
    BucketNotAllowed(String),
    InvalidBucket(String),
    InvalidProfile(String),
    ProfileNotAllowed(String),
}

impl fmt::Display for S3SourcePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BucketNotAllowed(bucket) => {
                write!(formatter, "S3 bucket is not allow-listed: {bucket}")
            }
            Self::InvalidBucket(bucket) => {
                write!(formatter, "invalid S3 bucket policy entry: {bucket:?}")
            }
            Self::InvalidProfile(profile) => {
                write!(formatter, "invalid S3 profile policy entry: {profile:?}")
            }
            Self::ProfileNotAllowed(profile) => write!(
                formatter,
                "S3 credential profile is not allow-listed: {profile}"
            ),
        }
    }
}

impl std::error::Error for S3SourcePolicyError {}

/// A bounded S3 OME-Zarr root. Credentials are resolved only by OpenDAL for the caller-selected,
/// policy-authorized profile; credential values never enter this API or Palace.
#[derive(Clone, Debug)]
pub struct S3ZarrStore {
    operator: Operator,
    bucket: String,
    prefix: String,
    max_asset_bytes: u64,
}

impl S3ZarrStore {
    pub fn new(
        policy: S3SourcePolicy,
        bucket: &str,
        prefix: &str,
        profile: &str,
        region: Option<&str>,
        max_asset_bytes: u64,
    ) -> Result<Self, S3StoreError> {
        if max_asset_bytes == 0 {
            return Err(S3StoreError::InvalidByteLimit);
        }
        policy
            .authorize(bucket, profile)
            .map_err(S3StoreError::Policy)?;
        let prefix = normalize_s3_prefix(prefix)?;
        let mut builder = services::S3::default()
            .bucket(bucket)
            .root(&prefix)
            .profile(profile);
        if let Some(region) = region.filter(|region| !region.is_empty()) {
            builder = builder.region(region);
        }
        let operator = Operator::new(builder).map_err(S3StoreError::Operator)?;
        Ok(Self {
            operator,
            bucket: bucket.to_owned(),
            prefix,
            max_asset_bytes,
        })
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn fetch_asset(&self, asset: &str) -> Result<Vec<u8>, S3StoreError> {
        if !is_normal_asset_path(asset) {
            return Err(S3StoreError::InvalidAssetPath(asset.to_owned()));
        }
        let metadata =
            pollster::block_on(self.operator.stat(asset)).map_err(S3StoreError::Operator)?;
        if metadata.content_length() > self.max_asset_bytes {
            return Err(S3StoreError::TooLarge);
        }
        let read_limit = self
            .max_asset_bytes
            .checked_add(1)
            .ok_or(S3StoreError::InvalidByteLimit)?;
        let bytes = pollster::block_on(self.operator.read_with(asset).range(0..read_limit))
            .map_err(S3StoreError::Operator)?;
        let bytes = bytes.to_bytes();
        if bytes.len() as u64 > self.max_asset_bytes {
            return Err(S3StoreError::TooLarge);
        }
        Ok(bytes.to_vec())
    }

    pub fn read_dataset_metadata(&self) -> Result<DatasetMetadata, S3StoreError> {
        match self.fetch_asset("zarr.json") {
            Ok(bytes) => parse_v3_root_metadata(&bytes).map_err(S3StoreError::Json),
            Err(S3StoreError::Operator(error)) if error.kind() == opendal::ErrorKind::NotFound => {
                let bytes = self.fetch_asset(".zattrs")?;
                serde_json::from_slice(&bytes).map_err(S3StoreError::Json)
            }
            Err(error) => Err(error),
        }
    }

    pub fn read_array_info(&self, relative_path: &str) -> Result<ArrayInfo, S3StoreError> {
        let v3 = format!("{relative_path}/zarr.json");
        match self.fetch_asset(&v3) {
            Ok(bytes) => parse_v3_array_info(&bytes).map_err(S3StoreError::Json),
            Err(S3StoreError::Operator(error)) if error.kind() == opendal::ErrorKind::NotFound => {
                let bytes = self.fetch_asset(&format!("{relative_path}/.zarray"))?;
                serde_json::from_slice(&bytes).map_err(S3StoreError::Json)
            }
            Err(error) => Err(error),
        }
    }
}

fn normalize_s3_prefix(prefix: &str) -> Result<String, S3StoreError> {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        return Ok(String::new());
    }
    if !is_normal_asset_path(prefix) {
        return Err(S3StoreError::InvalidPrefix(prefix.to_owned()));
    }
    Ok(prefix.to_owned())
}

#[derive(Debug)]
pub enum S3StoreError {
    InvalidAssetPath(String),
    InvalidByteLimit,
    InvalidPrefix(String),
    Json(serde_json::Error),
    Operator(opendal::Error),
    Policy(S3SourcePolicyError),
    TooLarge,
}

impl fmt::Display for S3StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAssetPath(path) => write!(
                formatter,
                "S3 Zarr asset path must be a non-empty normal relative path: {path:?}"
            ),
            Self::InvalidByteLimit => {
                write!(formatter, "S3 Zarr asset byte limit must be non-zero")
            }
            Self::InvalidPrefix(prefix) => write!(
                formatter,
                "S3 Zarr root prefix must be empty or normal relative path: {prefix:?}"
            ),
            Self::Json(error) => write!(formatter, "invalid S3 OME-Zarr metadata: {error}"),
            Self::Operator(error) => write!(formatter, "S3 Zarr operation failed: {error}"),
            Self::Policy(error) => write!(formatter, "S3 source rejected: {error}"),
            Self::TooLarge => write!(formatter, "S3 Zarr asset exceeds the configured size limit"),
        }
    }
}

impl std::error::Error for S3StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Operator(error) => Some(error),
            Self::Policy(error) => Some(error),
            Self::InvalidAssetPath(_)
            | Self::InvalidByteLimit
            | Self::InvalidPrefix(_)
            | Self::TooLarge => None,
        }
    }
}

/// An opened dataset source with its authorization decision attached. This deliberately keeps
/// provider credentials and renderer integration out of Palace: callers may use the same
/// metadata-facing source vocabulary for a local root or an allow-listed HTTPS Zarr root.
#[derive(Clone, Debug)]
pub enum DatasetSource {
    Local(PathBuf),
    Https(RemoteZarrStore),
    S3(S3ZarrStore),
}

impl DatasetSource {
    pub fn read_dataset_metadata(&self) -> Result<DatasetMetadata, DatasetSourceError> {
        match self {
            Self::Local(root) => read_dataset_metadata(root).map_err(DatasetSourceError::Local),
            Self::Https(store) => store
                .read_dataset_metadata()
                .map_err(DatasetSourceError::Remote),
            Self::S3(store) => store
                .read_dataset_metadata()
                .map_err(DatasetSourceError::S3),
        }
    }

    /// Reads compact array metadata at a normal Zarr-relative path. The local and HTTPS variants
    /// each enforce their own root-containment rules before opening an asset.
    pub fn read_array_info(&self, relative_path: &str) -> Result<ArrayInfo, DatasetSourceError> {
        match self {
            Self::Local(root) => {
                read_array_info(root, relative_path).map_err(DatasetSourceError::Local)
            }
            Self::Https(store) => store
                .read_array_info(relative_path)
                .map_err(DatasetSourceError::Remote),
            Self::S3(store) => store
                .read_array_info(relative_path)
                .map_err(DatasetSourceError::S3),
        }
    }

    /// Fetch a bounded, normal Zarr-relative asset after the source's local or HTTPS
    /// authorization decision. This is the raw-asset boundary used by chunk decoders; it does
    /// not interpret codecs or permit a caller to escape the opened store root.
    pub fn fetch_asset(&self, asset: &str) -> Result<Vec<u8>, DatasetAssetError> {
        match self {
            Self::Local(root) => read_local_asset(root, asset, MAX_REMOTE_ASSET_BYTES)
                .map_err(DatasetAssetError::Local),
            Self::Https(store) => store.fetch_asset(asset).map_err(DatasetAssetError::Remote),
            Self::S3(store) => store.fetch_asset(asset).map_err(DatasetAssetError::S3),
        }
    }
}

#[derive(Debug)]
pub enum DatasetAssetError {
    Local(LocalAssetError),
    Remote(RemoteStoreError),
    S3(S3StoreError),
}

impl fmt::Display for DatasetAssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(error) => write!(formatter, "local dataset asset failed: {error}"),
            Self::Remote(error) => write!(formatter, "HTTPS dataset asset failed: {error}"),
            Self::S3(error) => write!(formatter, "S3 dataset asset failed: {error}"),
        }
    }
}

impl std::error::Error for DatasetAssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Local(error) => Some(error),
            Self::Remote(error) => Some(error),
            Self::S3(error) => Some(error),
        }
    }
}

/// A fully assembled, row-major Zarr v3 `uint16` volume. Dimensions and voxel order remain
/// NGFF/Zarr `[z, y, x]`; renderers must make any API-specific axis conversion explicit.
#[derive(Clone, Debug, PartialEq)]
pub struct RawV3U16Volume {
    pub dimensions_zyx: [usize; 3],
    pub voxels: Vec<u16>,
}

/// Assemble a regular, default-keyed Zarr v3 volume with the portable `bytes` little-endian
/// codec. This is intentionally a narrow raw-data boundary: compressed codecs are rejected here
/// so a caller cannot accidentally treat compressed bytes as pixels.
pub fn read_v3_raw_u16_volume(
    source: &DatasetSource,
    array_path: &str,
    max_voxel_bytes: u64,
) -> Result<RawV3U16Volume, RawV3U16VolumeError> {
    if max_voxel_bytes == 0 {
        return Err(RawV3U16VolumeError::InvalidByteLimit);
    }
    let metadata_asset = format!("{array_path}/zarr.json");
    let metadata = source
        .fetch_asset(&metadata_asset)
        .map_err(RawV3U16VolumeError::Asset)?;
    let layout = parse_v3_raw_u16_layout(&metadata).map_err(RawV3U16VolumeError::Layout)?;
    let voxel_count = layout
        .shape
        .iter()
        .try_fold(1_usize, |count, size| count.checked_mul(*size))
        .ok_or(RawV3U16VolumeError::Layout(
            "volume shape overflows addressable memory".into(),
        ))?;
    let voxel_bytes =
        voxel_count
            .checked_mul(std::mem::size_of::<u16>())
            .ok_or(RawV3U16VolumeError::Layout(
                "volume byte length overflows addressable memory".into(),
            ))?;
    if voxel_bytes as u64 > max_voxel_bytes {
        return Err(RawV3U16VolumeError::OutputTooLarge {
            bytes: voxel_bytes as u64,
            limit: max_voxel_bytes,
        });
    }
    let chunk_bytes = layout
        .chunks
        .iter()
        .try_fold(1_usize, |count, size| count.checked_mul(*size))
        .and_then(|count| count.checked_mul(std::mem::size_of::<u16>()))
        .ok_or(RawV3U16VolumeError::Layout(
            "chunk byte length overflows addressable memory".into(),
        ))?;
    let chunk_counts: [usize; 3] =
        std::array::from_fn(|axis| layout.shape[axis].div_ceil(layout.chunks[axis]));
    let mut voxels = vec![0_u16; voxel_count];
    for chunk_z in 0..chunk_counts[0] {
        for chunk_y in 0..chunk_counts[1] {
            for chunk_x in 0..chunk_counts[2] {
                let asset = format!("{array_path}/c/{chunk_z}/{chunk_y}/{chunk_x}");
                let bytes = source
                    .fetch_asset(&asset)
                    .map_err(RawV3U16VolumeError::Asset)?;
                if bytes.len() != chunk_bytes {
                    return Err(RawV3U16VolumeError::InvalidChunkLength {
                        asset,
                        actual: bytes.len(),
                        expected: chunk_bytes,
                    });
                }
                let (words, remainder) = bytes.as_chunks::<2>();
                debug_assert!(remainder.is_empty());
                for local_z in 0..layout.chunks[0] {
                    let z = chunk_z * layout.chunks[0] + local_z;
                    if z >= layout.shape[0] {
                        continue;
                    }
                    for local_y in 0..layout.chunks[1] {
                        let y = chunk_y * layout.chunks[1] + local_y;
                        if y >= layout.shape[1] {
                            continue;
                        }
                        for local_x in 0..layout.chunks[2] {
                            let x = chunk_x * layout.chunks[2] + local_x;
                            if x >= layout.shape[2] {
                                continue;
                            }
                            let local =
                                local_x + layout.chunks[2] * (local_y + layout.chunks[1] * local_z);
                            let global = x + layout.shape[2] * (y + layout.shape[1] * z);
                            voxels[global] = u16::from_le_bytes(words[local]);
                        }
                    }
                }
            }
        }
    }
    Ok(RawV3U16Volume {
        dimensions_zyx: layout.shape,
        voxels,
    })
}

struct RawV3U16Layout {
    shape: [usize; 3],
    chunks: [usize; 3],
}

fn parse_v3_raw_u16_layout(bytes: &[u8]) -> Result<RawV3U16Layout, String> {
    let metadata: ZarrV3RawArray =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if metadata.data_type != "uint16" {
        return Err(format!(
            "expected uint16 data_type, found {}",
            metadata.data_type
        ));
    }
    if metadata.chunk_grid.name != "regular" {
        return Err(format!(
            "expected regular chunk grid, found {}",
            metadata.chunk_grid.name
        ));
    }
    let shape = three_usize(&metadata.shape, "shape")?;
    let chunks = three_usize(
        &metadata.chunk_grid.configuration.chunk_shape,
        "chunk shape",
    )?;
    if chunks.contains(&0) {
        return Err("chunk dimensions must be non-zero".into());
    }
    let key_encoding = metadata
        .chunk_key_encoding
        .ok_or_else(|| "missing chunk_key_encoding".to_owned())?;
    if key_encoding.name != "default"
        || key_encoding.configuration.separator.as_deref() != Some("/")
    {
        return Err("only default slash-separated Zarr v3 chunk keys are supported".into());
    }
    if metadata.codecs.len() != 1
        || metadata.codecs[0].name != "bytes"
        || metadata.codecs[0].configuration.endian.as_deref() != Some("little")
    {
        return Err("only one little-endian Zarr v3 bytes codec is supported".into());
    }
    Ok(RawV3U16Layout { shape, chunks })
}

fn three_usize(values: &[u64], name: &str) -> Result<[usize; 3], String> {
    let values: Vec<_> = values
        .iter()
        .map(|value| usize::try_from(*value).map_err(|_| ()))
        .collect::<Result<_, _>>()
        .map_err(|_| format!("{name} contains a value too large for this platform"))?;
    let values: [usize; 3] = values
        .try_into()
        .map_err(|_| format!("{name} must have exactly three Z,Y,X dimensions"))?;
    if values.contains(&0) {
        return Err(format!("{name} dimensions must be non-zero"));
    }
    Ok(values)
}

#[derive(Debug)]
pub enum RawV3U16VolumeError {
    Asset(DatasetAssetError),
    InvalidByteLimit,
    InvalidChunkLength {
        asset: String,
        actual: usize,
        expected: usize,
    },
    Layout(String),
    OutputTooLarge {
        bytes: u64,
        limit: u64,
    },
}

impl fmt::Display for RawV3U16VolumeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Asset(error) => write!(formatter, "raw Zarr volume asset failed: {error}"),
            Self::InvalidByteLimit => {
                write!(formatter, "raw Zarr volume byte limit must be non-zero")
            }
            Self::InvalidChunkLength {
                asset,
                actual,
                expected,
            } => write!(
                formatter,
                "raw Zarr chunk {asset} has {actual} bytes; expected {expected}"
            ),
            Self::Layout(error) => write!(formatter, "unsupported raw Zarr v3 layout: {error}"),
            Self::OutputTooLarge { bytes, limit } => write!(
                formatter,
                "raw Zarr volume requires {bytes} bytes, exceeding the {limit}-byte limit"
            ),
        }
    }
}

impl std::error::Error for RawV3U16VolumeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Asset(error) => Some(error),
            Self::InvalidByteLimit
            | Self::InvalidChunkLength { .. }
            | Self::Layout(_)
            | Self::OutputTooLarge { .. } => None,
        }
    }
}

/// The application-owned registry for source capabilities. It accepts no implicit filesystem or
/// network authority: each local root is canonicalized against `local`, and every HTTPS root is
/// checked against `remote` before a [`DatasetSource`] is returned.
#[derive(Clone, Debug)]
pub struct SourceRegistry {
    local: LocalSourcePolicy,
    remote: RemoteSourcePolicy,
    s3: S3SourcePolicy,
    remote_asset_byte_limit: u64,
}

impl SourceRegistry {
    pub fn new(
        local: LocalSourcePolicy,
        remote: RemoteSourcePolicy,
        remote_asset_byte_limit: u64,
    ) -> Result<Self, SourceRegistryError> {
        if remote_asset_byte_limit == 0 {
            return Err(SourceRegistryError::InvalidRemoteAssetByteLimit);
        }
        Ok(Self {
            local,
            remote,
            s3: S3SourcePolicy::default(),
            remote_asset_byte_limit,
        })
    }

    pub fn open_local(&self, root: impl AsRef<Path>) -> Result<DatasetSource, SourceRegistryError> {
        self.local
            .authorize(root)
            .map(DatasetSource::Local)
            .map_err(SourceRegistryError::Local)
    }

    pub fn open_https(&self, root: &str) -> Result<DatasetSource, SourceRegistryError> {
        RemoteZarrStore::new(self.remote.clone(), root, self.remote_asset_byte_limit)
            .map(DatasetSource::Https)
            .map_err(SourceRegistryError::Remote)
    }

    /// Add an explicit S3 capability policy. Keeping it on the registry means a caller cannot
    /// accidentally use an ambient AWS profile or arbitrary bucket through a generic URL path.
    pub fn with_s3_policy(mut self, policy: S3SourcePolicy) -> Self {
        self.s3 = policy;
        self
    }

    pub fn open_s3(
        &self,
        bucket: &str,
        prefix: &str,
        profile: &str,
        region: Option<&str>,
    ) -> Result<DatasetSource, SourceRegistryError> {
        S3ZarrStore::new(
            self.s3.clone(),
            bucket,
            prefix,
            profile,
            region,
            self.remote_asset_byte_limit,
        )
        .map(DatasetSource::S3)
        .map_err(SourceRegistryError::S3)
    }
}

#[derive(Debug)]
pub enum DatasetSourceError {
    Local(MetadataError),
    Remote(RemoteStoreError),
    S3(S3StoreError),
}

impl fmt::Display for DatasetSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(error) => write!(formatter, "local dataset source failed: {error}"),
            Self::Remote(error) => write!(formatter, "HTTPS dataset source failed: {error}"),
            Self::S3(error) => write!(formatter, "S3 dataset source failed: {error}"),
        }
    }
}

impl std::error::Error for DatasetSourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Local(error) => Some(error),
            Self::Remote(error) => Some(error),
            Self::S3(error) => Some(error),
        }
    }
}

#[derive(Debug)]
pub enum SourceRegistryError {
    InvalidRemoteAssetByteLimit,
    Local(SourcePolicyError),
    Remote(RemoteStoreError),
    S3(S3StoreError),
}

impl fmt::Display for SourceRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRemoteAssetByteLimit => {
                write!(formatter, "remote source asset byte limit must be non-zero")
            }
            Self::Local(error) => write!(formatter, "local dataset source rejected: {error}"),
            Self::Remote(error) => write!(formatter, "HTTPS dataset source rejected: {error}"),
            Self::S3(error) => write!(formatter, "S3 dataset source rejected: {error}"),
        }
    }
}

impl std::error::Error for SourceRegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Local(error) => Some(error),
            Self::Remote(error) => Some(error),
            Self::S3(error) => Some(error),
            Self::InvalidRemoteAssetByteLimit => None,
        }
    }
}

#[derive(Debug)]
pub enum RemoteMetadataError {
    Fetch(RemoteFetchError),
    InvalidRootUrl(Url),
    Json(serde_json::Error),
    Policy(RemoteSourcePolicyError),
    Url(url::ParseError),
}

impl fmt::Display for RemoteMetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(error) => write!(f, "remote OME-Zarr metadata fetch failed: {error}"),
            Self::InvalidRootUrl(url) => write!(
                f,
                "remote OME-Zarr root cannot contain a query or fragment: {url}"
            ),
            Self::Json(error) => write!(f, "invalid remote OME-Zarr metadata: {error}"),
            Self::Policy(error) => write!(f, "remote OME-Zarr root rejected: {error}"),
            Self::Url(error) => write!(f, "invalid remote OME-Zarr metadata URL: {error}"),
        }
    }
}

impl std::error::Error for RemoteMetadataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Fetch(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Policy(error) => Some(error),
            Self::Url(error) => Some(error),
            Self::InvalidRootUrl(_) => None,
        }
    }
}

/// Reads OME-NGFF group attributes from a local Zarr v3 root (`zarr.json`) or legacy Zarr v2
/// root (`.zattrs`). Array chunk loading belongs to a store implementation; this function is
/// intentionally limited to the small metadata boundary.
pub fn read_dataset_metadata(root: impl AsRef<Path>) -> Result<DatasetMetadata, MetadataError> {
    let root = root.as_ref();
    let v3 = root.join("zarr.json");
    match read_bounded(&v3) {
        Ok(bytes) => parse_v3_root_metadata(&bytes).map_err(MetadataError::Json),
        Err(MetadataError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            let v2 = root.join(".zattrs");
            let bytes = read_bounded(&v2)?;
            serde_json::from_slice(&bytes).map_err(MetadataError::Json)
        }
        Err(error) => Err(error),
    }
}

/// Read an array's compact shape/chunk/dtype metadata from either Zarr v3 (`zarr.json`) or
/// Zarr v2 (`.zarray`). `relative_path` is the NGFF dataset path, not an unrestricted path.
pub fn read_array_info(
    root: impl AsRef<Path>,
    relative_path: impl AsRef<Path>,
) -> Result<ArrayInfo, MetadataError> {
    let root = root.as_ref();
    let relative_path = relative_path.as_ref();
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(MetadataError::InvalidArrayPath(relative_path.to_path_buf()));
    }
    let directory = root.join(relative_path);
    let v3 = directory.join("zarr.json");
    match read_bounded(&v3) {
        Ok(bytes) => parse_v3_array_info(&bytes).map_err(MetadataError::Json),
        Err(MetadataError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            let bytes = read_bounded(&directory.join(".zarray"))?;
            serde_json::from_slice(&bytes).map_err(MetadataError::Json)
        }
        Err(error) => Err(error),
    }
}

/// Read one raw Zarr asset below a local root under a caller-selected byte cap. The asset must
/// consist entirely of normal path components; absolute paths, traversal, query-like characters,
/// and percent escapes are rejected before filesystem access.
pub fn read_local_asset(
    root: impl AsRef<Path>,
    asset: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, LocalAssetError> {
    if max_bytes == 0 {
        return Err(LocalAssetError::InvalidByteLimit);
    }
    if !is_normal_asset_path(asset) {
        return Err(LocalAssetError::InvalidAssetPath(asset.to_owned()));
    }
    let root = root.as_ref().canonicalize().map_err(LocalAssetError::Io)?;
    let path = root
        .join(asset)
        .canonicalize()
        .map_err(LocalAssetError::Io)?;
    if !path.starts_with(&root) {
        return Err(LocalAssetError::EscapesRoot(path));
    }
    let mut file = File::open(&path).map_err(LocalAssetError::Io)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(LocalAssetError::Io)?;
    if bytes.len() as u64 > max_bytes {
        return Err(LocalAssetError::TooLarge {
            path,
            limit: max_bytes,
        });
    }
    Ok(bytes)
}

fn is_normal_asset_path(asset: &str) -> bool {
    !asset.is_empty()
        && !asset.contains(['%', '?', '#', '\\'])
        && !Path::new(asset).is_absolute()
        && Path::new(asset)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

#[derive(Debug)]
pub enum LocalAssetError {
    EscapesRoot(PathBuf),
    InvalidAssetPath(String),
    InvalidByteLimit,
    Io(std::io::Error),
    TooLarge { path: PathBuf, limit: u64 },
}

impl fmt::Display for LocalAssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EscapesRoot(path) => write!(
                formatter,
                "local Zarr asset resolves outside its authorized root: {}",
                path.display()
            ),
            Self::InvalidAssetPath(asset) => write!(
                formatter,
                "local Zarr asset path must be a non-empty normal relative path: {asset:?}"
            ),
            Self::InvalidByteLimit => {
                write!(formatter, "local Zarr asset byte limit must be non-zero")
            }
            Self::Io(error) => write!(formatter, "local Zarr asset IO error: {error}"),
            Self::TooLarge { path, limit } => write!(
                formatter,
                "local Zarr asset {} exceeds the {limit}-byte limit",
                path.display()
            ),
        }
    }
}

impl std::error::Error for LocalAssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::EscapesRoot(_)
            | Self::InvalidAssetPath(_)
            | Self::InvalidByteLimit
            | Self::TooLarge { .. } => None,
        }
    }
}

#[derive(Deserialize)]
struct ZarrV3Root {
    #[serde(default)]
    attributes: DatasetMetadata,
}

#[derive(Deserialize)]
struct ZarrV3Array {
    shape: Vec<u64>,
    data_type: String,
    chunk_grid: ZarrV3ChunkGrid,
}

#[derive(Deserialize)]
struct ZarrV3RawArray {
    shape: Vec<u64>,
    data_type: String,
    chunk_grid: ZarrV3RawChunkGrid,
    chunk_key_encoding: Option<ZarrV3ChunkKeyEncoding>,
    codecs: Vec<ZarrV3Codec>,
}

#[derive(Deserialize)]
struct ZarrV3RawChunkGrid {
    name: String,
    configuration: ZarrV3ChunkGridConfiguration,
}

#[derive(Deserialize)]
struct ZarrV3ChunkKeyEncoding {
    name: String,
    configuration: ZarrV3ChunkKeyEncodingConfiguration,
}

#[derive(Deserialize)]
struct ZarrV3ChunkKeyEncodingConfiguration {
    separator: Option<String>,
}

#[derive(Deserialize)]
struct ZarrV3Codec {
    name: String,
    configuration: ZarrV3CodecConfiguration,
}

#[derive(Deserialize)]
struct ZarrV3CodecConfiguration {
    endian: Option<String>,
}

#[derive(Deserialize)]
struct ZarrV3ChunkGrid {
    configuration: ZarrV3ChunkGridConfiguration,
}

#[derive(Deserialize)]
struct ZarrV3ChunkGridConfiguration {
    chunk_shape: Vec<u64>,
}

/// Parsing is independent of the transport. Local files and a capability-scoped remote store
/// therefore agree on the exact supported Zarr v3 metadata subset.
fn parse_v3_root_metadata(bytes: &[u8]) -> Result<DatasetMetadata, serde_json::Error> {
    let document: ZarrV3Root = serde_json::from_slice(bytes)?;
    Ok(document.attributes)
}

fn parse_v3_array_info(bytes: &[u8]) -> Result<ArrayInfo, serde_json::Error> {
    let document: ZarrV3Array = serde_json::from_slice(bytes)?;
    Ok(ArrayInfo {
        shape: document.shape,
        chunks: document.chunk_grid.configuration.chunk_shape,
        dtype: document.data_type,
        order: None,
        compressor: None,
    })
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, MetadataError> {
    let mut file = File::open(path).map_err(MetadataError::Io)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_ROOT_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(MetadataError::Io)?;
    if bytes.len() as u64 > MAX_ROOT_METADATA_BYTES {
        return Err(MetadataError::TooLarge {
            path: path.to_path_buf(),
            limit: MAX_ROOT_METADATA_BYTES,
        });
    }
    Ok(bytes)
}

#[derive(Debug)]
pub enum MetadataError {
    Io(std::io::Error),
    Json(serde_json::Error),
    TooLarge { path: PathBuf, limit: u64 },
    InvalidArrayPath(PathBuf),
}

impl fmt::Display for MetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "metadata IO error: {error}"),
            Self::Json(error) => write!(formatter, "invalid OME-Zarr metadata: {error}"),
            Self::TooLarge { path, limit } => write!(
                formatter,
                "metadata file {} exceeds the {limit}-byte limit",
                path.display()
            ),
            Self::InvalidArrayPath(path) => write!(
                formatter,
                "array path must be relative and stay below its Zarr root: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for MetadataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::TooLarge { .. } | Self::InvalidArrayPath(_) => None,
        }
    }
}

/// A dense homogeneous matrix. `matrix[row][column]` maps an `n`-element input into an
/// `n`-element output through an `(n + 1) × (n + 1)` affine matrix.
#[derive(Clone, Debug, PartialEq)]
pub struct AffineTransform {
    matrix: Vec<Vec<f64>>,
}

impl AffineTransform {
    pub fn identity(dimensions: usize) -> Self {
        let mut matrix = vec![vec![0.0; dimensions + 1]; dimensions + 1];
        for (index, row) in matrix.iter_mut().enumerate() {
            row[index] = 1.0;
        }
        Self { matrix }
    }

    pub fn from_matrix(matrix: Vec<Vec<f64>>) -> Result<Self, TransformError> {
        let rows = matrix.len();
        if rows < 2 || matrix.iter().any(|row| row.len() != rows) {
            return Err(TransformError::InvalidAffineShape);
        }
        if matrix.iter().flatten().any(|value| !value.is_finite()) {
            return Err(TransformError::NonFiniteValue);
        }
        Ok(Self { matrix })
    }

    pub fn dimensions(&self) -> usize {
        self.matrix.len() - 1
    }

    pub fn matrix(&self) -> &[Vec<f64>] {
        &self.matrix
    }

    pub fn apply(&self, point: &[f64]) -> Result<Vec<f64>, TransformError> {
        if point.len() != self.dimensions() {
            return Err(TransformError::DimensionMismatch {
                expected: self.dimensions(),
                actual: point.len(),
            });
        }
        if point.iter().any(|value| !value.is_finite()) {
            return Err(TransformError::NonFiniteValue);
        }
        let homogeneous = point.iter().copied().chain(std::iter::once(1.0));
        let input: Vec<_> = homogeneous.collect();
        Ok(self.matrix[..self.dimensions()]
            .iter()
            .map(|row| row.iter().zip(&input).map(|(a, b)| a * b).sum())
            .collect())
    }

    /// Maps a physical point back to source coordinates when the affine's linear component is
    /// invertible. This is used for display-only annotation projections; physical coordinates
    /// remain the annotation authority.
    pub fn inverse_apply(&self, point: &[f64]) -> Result<Vec<f64>, TransformError> {
        let dimensions = self.dimensions();
        if point.len() != dimensions {
            return Err(TransformError::DimensionMismatch {
                expected: dimensions,
                actual: point.len(),
            });
        }
        if point.iter().any(|value| !value.is_finite()) {
            return Err(TransformError::NonFiniteValue);
        }
        let mut augmented = vec![vec![0.0; dimensions + 1]; dimensions];
        for (row_index, row) in augmented.iter_mut().enumerate() {
            for (column, value) in row.iter_mut().take(dimensions).enumerate() {
                *value = self.matrix[row_index][column];
            }
            row[dimensions] = point[row_index] - self.matrix[row_index][dimensions];
        }
        for pivot in 0..dimensions {
            let best = (pivot..dimensions)
                .max_by(|left, right| {
                    augmented[*left][pivot]
                        .abs()
                        .total_cmp(&augmented[*right][pivot].abs())
                })
                .expect("pivot range is non-empty");
            if augmented[best][pivot].abs() <= f64::EPSILON {
                return Err(TransformError::NonInvertible);
            }
            augmented.swap(pivot, best);
            let divisor = augmented[pivot][pivot];
            for value in &mut augmented[pivot][pivot..] {
                *value /= divisor;
            }
            let pivot_row = augmented[pivot].clone();
            for (row_index, row) in augmented.iter_mut().enumerate() {
                if row_index == pivot {
                    continue;
                }
                let factor = row[pivot];
                for (value, pivot_value) in row[pivot..].iter_mut().zip(&pivot_row[pivot..]) {
                    *value -= factor * pivot_value;
                }
            }
        }
        Ok(augmented.into_iter().map(|row| row[dimensions]).collect())
    }

    /// Returns `next ∘ self`: apply `self`, then apply `next`.
    pub fn then(&self, next: &Self) -> Result<Self, TransformError> {
        if self.dimensions() != next.dimensions() {
            return Err(TransformError::DimensionMismatch {
                expected: self.dimensions(),
                actual: next.dimensions(),
            });
        }
        let size = self.matrix.len();
        let mut result = vec![vec![0.0; size]; size];
        for (row, result_row) in result.iter_mut().enumerate() {
            for (column, cell) in result_row.iter_mut().enumerate() {
                *cell = (0..size)
                    .map(|index| next.matrix[row][index] * self.matrix[index][column])
                    .sum();
            }
        }
        Self::from_matrix(result)
    }

    pub fn from_operations(
        dimensions: usize,
        operations: &[CoordinateTransformation],
    ) -> Result<Self, TransformError> {
        let mut result = Self::identity(dimensions);
        for operation in operations {
            let operation = match operation {
                CoordinateTransformation::Scale { scale } => Self::scale(dimensions, scale)?,
                CoordinateTransformation::Translation { translation } => {
                    Self::translation(dimensions, translation)?
                }
                CoordinateTransformation::Affine { matrix } => Self::from_matrix(matrix.clone())?,
            };
            result = result.then(&operation)?;
        }
        Ok(result)
    }

    fn scale(dimensions: usize, scale: &[f64]) -> Result<Self, TransformError> {
        check_operation_values(dimensions, scale)?;
        let mut result = Self::identity(dimensions);
        for (axis, value) in scale.iter().enumerate() {
            result.matrix[axis][axis] = *value;
        }
        Ok(result)
    }

    fn translation(dimensions: usize, translation: &[f64]) -> Result<Self, TransformError> {
        check_operation_values(dimensions, translation)?;
        let mut result = Self::identity(dimensions);
        for (axis, value) in translation.iter().enumerate() {
            result.matrix[axis][dimensions] = *value;
        }
        Ok(result)
    }
}

fn check_operation_values(dimensions: usize, values: &[f64]) -> Result<(), TransformError> {
    if values.len() != dimensions {
        return Err(TransformError::DimensionMismatch {
            expected: dimensions,
            actual: values.len(),
        });
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err(TransformError::NonFiniteValue);
    }
    Ok(())
}

/// A named-coordinate-system graph. Edges are directed because an affine transform is not
/// necessarily invertible (for example, a projection or displacement-derived transform).
#[derive(Clone, Debug, Default)]
pub struct TransformGraph {
    systems: HashSet<String>,
    edges: Vec<TransformEdge>,
}

#[derive(Clone, Debug)]
pub struct TransformEdge {
    pub from: String,
    pub to: String,
    pub transform: AffineTransform,
}

impl TransformGraph {
    pub fn add_system(&mut self, name: impl Into<String>) -> Result<(), TransformError> {
        let name = name.into();
        if name.is_empty() {
            return Err(TransformError::EmptyCoordinateSystem);
        }
        self.systems.insert(name);
        Ok(())
    }

    pub fn add_edge(&mut self, edge: TransformEdge) -> Result<(), TransformError> {
        if !self.systems.contains(&edge.from) || !self.systems.contains(&edge.to) {
            return Err(TransformError::UnknownCoordinateSystem);
        }
        self.edges.push(edge);
        Ok(())
    }

    /// Finds a directed route and returns its composed transform.
    pub fn transform(&self, from: &str, to: &str) -> Result<AffineTransform, TransformError> {
        if !self.systems.contains(from) || !self.systems.contains(to) {
            return Err(TransformError::UnknownCoordinateSystem);
        }
        if from == to {
            let dimensions = self
                .edges
                .iter()
                .find(|edge| edge.from == from || edge.to == from)
                .map(|edge| edge.transform.dimensions())
                .ok_or(TransformError::DimensionUnknown)?;
            return Ok(AffineTransform::identity(dimensions));
        }

        let mut queue: VecDeque<(String, Option<AffineTransform>)> =
            VecDeque::from([(from.to_owned(), None)]);
        let mut visited = HashSet::from([from.to_owned()]);
        while let Some((system, accumulated)) = queue.pop_front() {
            for edge in self.edges.iter().filter(|edge| edge.from == system) {
                let transform = match accumulated.as_ref() {
                    Some(previous) => previous.then(&edge.transform)?,
                    None => edge.transform.clone(),
                };
                if edge.to == to {
                    return Ok(transform);
                }
                if visited.insert(edge.to.clone()) {
                    queue.push_back((edge.to.clone(), Some(transform)));
                }
            }
        }
        Err(TransformError::NoTransformPath {
            from: from.to_owned(),
            to: to.to_owned(),
        })
    }
}

/// Composes a level's transform first, then the transform shared by all levels in a multiscale.
pub fn level_transform(
    multiscale: &Multiscale,
    level: usize,
) -> Result<AffineTransform, TransformError> {
    let dataset = multiscale
        .datasets
        .get(level)
        .ok_or(TransformError::UnknownLevel(level))?;
    let dimensions = multiscale.axes.len();
    let local = AffineTransform::from_operations(dimensions, &dataset.coordinate_transformations)?;
    let shared =
        AffineTransform::from_operations(dimensions, &multiscale.coordinate_transformations)?;
    local.then(&shared)
}

/// One resolution level in an anisotropic multiscale pyramid.
///
/// `factors` are cumulative factors relative to level zero, not the single step from the
/// preceding level. This makes the physical voxel size and every level's NGFF scale explicit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PyramidLevel {
    pub factors: [u64; 3],
    pub shape: [u64; 3],
}

/// Proposes a BigDataViewer-style anisotropic pyramid schedule for `[x, y, z]` data.
///
/// At each level it halves axes whose *current physical voxel size* is within a factor of two
/// of the finest axis. Thus a 10:1 volume starts `[1,1,1] → [2,2,1] → [4,4,1] → [8,8,1] →
/// [16,16,2]`, rather than prematurely blurring z or pretending all levels have one scalar
/// scale. Levels stop once every dimension is at most `max_size` samples.
pub fn propose_pyramid(
    base_shape: [u64; 3],
    voxel_size: [f64; 3],
    max_size: u64,
) -> Result<Vec<PyramidLevel>, PyramidError> {
    if max_size == 0 {
        return Err(PyramidError::InvalidMaxSize);
    }
    if base_shape.contains(&0) {
        return Err(PyramidError::ZeroDimension);
    }
    if voxel_size
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return Err(PyramidError::InvalidVoxelSize(voxel_size));
    }

    let mut factors = [1_u64; 3];
    let mut levels = vec![PyramidLevel {
        factors,
        shape: level_shape(base_shape, factors),
    }];
    while levels
        .last()
        .expect("one level is always present")
        .shape
        .iter()
        .any(|&size| size > max_size)
    {
        let spacing: [f64; 3] = std::array::from_fn(|axis| voxel_size[axis] * factors[axis] as f64);
        let finest = spacing.iter().copied().fold(f64::INFINITY, f64::min);
        let current_shape = levels.last().expect("one level is always present").shape;
        let mut advanced = false;
        for axis in 0..3 {
            if current_shape[axis] > 1 && spacing[axis] / finest <= 2.0 {
                factors[axis] = factors[axis]
                    .checked_mul(2)
                    .ok_or(PyramidError::FactorOverflow)?;
                advanced = true;
            }
        }
        if !advanced {
            return Err(PyramidError::CannotReduceFurther);
        }
        levels.push(PyramidLevel {
            factors,
            shape: level_shape(base_shape, factors),
        });
    }
    Ok(levels)
}

/// Write a locally consumable, raw Zarr v3 OME-NGFF pyramid from a dense level-zero volume.
///
/// This deliberately writes the same narrow `uint16`/little-endian/regular-chunk subset that
/// [`read_v3_raw_u16_volume`] accepts. It is a deterministic fixture and small-data builder, not
/// a TB-scale streaming or compressed production writer: callers must already hold level zero in
/// memory and the destination must be absent or empty. `voxel_size_zyx` and `chunk_shape_zyx`
/// use the on-disk NGFF order.
pub fn write_v3_raw_u16_pyramid(
    root: impl AsRef<Path>,
    level_zero: &RawV3U16Volume,
    voxel_size_zyx: [f64; 3],
    chunk_shape_zyx: [usize; 3],
    max_size: u64,
) -> Result<Vec<PyramidLevel>, PyramidWriteError> {
    if level_zero.dimensions_zyx.contains(&0)
        || level_zero.voxels.len()
            != level_zero.dimensions_zyx[0]
                .checked_mul(level_zero.dimensions_zyx[1])
                .and_then(|count| count.checked_mul(level_zero.dimensions_zyx[2]))
                .ok_or(PyramidWriteError::VolumeShapeOverflow)?
    {
        return Err(PyramidWriteError::InvalidVolume);
    }
    if chunk_shape_zyx.contains(&0) {
        return Err(PyramidWriteError::InvalidChunkShape(chunk_shape_zyx));
    }
    checked_chunk_byte_len(chunk_shape_zyx)?;
    if voxel_size_zyx
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return Err(PyramidWriteError::InvalidVoxelSize(voxel_size_zyx));
    }
    let base_shape_xyz = [
        u64::try_from(level_zero.dimensions_zyx[2])
            .map_err(|_| PyramidWriteError::VolumeShapeOverflow)?,
        u64::try_from(level_zero.dimensions_zyx[1])
            .map_err(|_| PyramidWriteError::VolumeShapeOverflow)?,
        u64::try_from(level_zero.dimensions_zyx[0])
            .map_err(|_| PyramidWriteError::VolumeShapeOverflow)?,
    ];
    let levels = propose_pyramid(
        base_shape_xyz,
        [voxel_size_zyx[2], voxel_size_zyx[1], voxel_size_zyx[0]],
        max_size,
    )
    .map_err(PyramidWriteError::Plan)?;
    let root = root.as_ref();
    prepare_empty_pyramid_root(root)?;
    let datasets: Vec<_> = levels
        .iter()
        .enumerate()
        .map(|(index, level)| {
            serde_json::json!({
                "path": index.to_string(),
                "coordinateTransformations": [{
                    "type": "scale",
                    "scale": [
                        voxel_size_zyx[0] * level.factors[2] as f64,
                        voxel_size_zyx[1] * level.factors[1] as f64,
                        voxel_size_zyx[2] * level.factors[0] as f64,
                    ],
                }],
            })
        })
        .collect();
    write_json(
        &root.join("zarr.json"),
        &serde_json::json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "multiscales": [{
                    "version": "0.4",
                    "axes": [
                        {"name": "z", "type": "space"},
                        {"name": "y", "type": "space"},
                        {"name": "x", "type": "space"},
                    ],
                    "datasets": datasets,
                }],
            },
        }),
    )?;
    for (index, level) in levels.iter().enumerate() {
        let volume = downsample_u16_from_level_zero(level_zero, level.factors)?;
        write_v3_raw_u16_array(&root.join(index.to_string()), &volume, chunk_shape_zyx)?;
    }
    Ok(levels)
}

fn prepare_empty_pyramid_root(root: &Path) -> Result<(), PyramidWriteError> {
    match fs::read_dir(root) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                Err(PyramidWriteError::DestinationNotEmpty(root.to_path_buf()))
            } else {
                Ok(())
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(root).map_err(PyramidWriteError::Io)
        }
        Err(error) => Err(PyramidWriteError::Io(error)),
    }
}

fn downsample_u16_from_level_zero(
    level_zero: &RawV3U16Volume,
    factors_xyz: [u64; 3],
) -> Result<RawV3U16Volume, PyramidWriteError> {
    let factor_xyz = [
        usize::try_from(factors_xyz[0]).map_err(|_| PyramidWriteError::FactorTooLarge)?,
        usize::try_from(factors_xyz[1]).map_err(|_| PyramidWriteError::FactorTooLarge)?,
        usize::try_from(factors_xyz[2]).map_err(|_| PyramidWriteError::FactorTooLarge)?,
    ];
    let input = level_zero.dimensions_zyx;
    let output = [
        input[0].div_ceil(factor_xyz[2]),
        input[1].div_ceil(factor_xyz[1]),
        input[2].div_ceil(factor_xyz[0]),
    ];
    let voxel_count = output[0]
        .checked_mul(output[1])
        .and_then(|count| count.checked_mul(output[2]))
        .ok_or(PyramidWriteError::VolumeShapeOverflow)?;
    let mut voxels = vec![0_u16; voxel_count];
    for z in 0..output[0] {
        for y in 0..output[1] {
            for x in 0..output[2] {
                let mut sum = 0_u128;
                let mut count = 0_u128;
                for source_z in z * factor_xyz[2]..((z + 1) * factor_xyz[2]).min(input[0]) {
                    for source_y in y * factor_xyz[1]..((y + 1) * factor_xyz[1]).min(input[1]) {
                        for source_x in x * factor_xyz[0]..((x + 1) * factor_xyz[0]).min(input[2]) {
                            let source = source_x + input[2] * (source_y + input[1] * source_z);
                            sum += u128::from(level_zero.voxels[source]);
                            count += 1;
                        }
                    }
                }
                let destination = x + output[2] * (y + output[1] * z);
                voxels[destination] = u16::try_from((sum + count / 2) / count)
                    .expect("the rounded mean of u16 values remains u16");
            }
        }
    }
    Ok(RawV3U16Volume {
        dimensions_zyx: output,
        voxels,
    })
}

fn write_v3_raw_u16_array(
    root: &Path,
    volume: &RawV3U16Volume,
    chunk_shape_zyx: [usize; 3],
) -> Result<(), PyramidWriteError> {
    let chunk_byte_len = checked_chunk_byte_len(chunk_shape_zyx)?;
    fs::create_dir_all(root.join("c")).map_err(PyramidWriteError::Io)?;
    write_json(
        &root.join("zarr.json"),
        &serde_json::json!({
            "zarr_format": 3,
            "node_type": "array",
            "shape": volume.dimensions_zyx,
            "data_type": "uint16",
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": chunk_shape_zyx}},
            "chunk_key_encoding": {"name": "default", "configuration": {"separator": "/"}},
            "fill_value": 0,
            "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}],
            "attributes": {},
        }),
    )?;
    let chunk_counts: [usize; 3] =
        std::array::from_fn(|axis| volume.dimensions_zyx[axis].div_ceil(chunk_shape_zyx[axis]));
    for chunk_z in 0..chunk_counts[0] {
        for chunk_y in 0..chunk_counts[1] {
            for chunk_x in 0..chunk_counts[2] {
                let mut bytes = vec![0_u8; chunk_byte_len];
                for local_z in 0..chunk_shape_zyx[0] {
                    let z = chunk_z * chunk_shape_zyx[0] + local_z;
                    if z >= volume.dimensions_zyx[0] {
                        continue;
                    }
                    for local_y in 0..chunk_shape_zyx[1] {
                        let y = chunk_y * chunk_shape_zyx[1] + local_y;
                        if y >= volume.dimensions_zyx[1] {
                            continue;
                        }
                        for local_x in 0..chunk_shape_zyx[2] {
                            let x = chunk_x * chunk_shape_zyx[2] + local_x;
                            if x >= volume.dimensions_zyx[2] {
                                continue;
                            }
                            let source =
                                x + volume.dimensions_zyx[2] * (y + volume.dimensions_zyx[1] * z);
                            let destination = local_x
                                + chunk_shape_zyx[2] * (local_y + chunk_shape_zyx[1] * local_z);
                            bytes[destination * 2..destination * 2 + 2]
                                .copy_from_slice(&volume.voxels[source].to_le_bytes());
                        }
                    }
                }
                let chunk = root
                    .join("c")
                    .join(chunk_z.to_string())
                    .join(chunk_y.to_string());
                fs::create_dir_all(&chunk).map_err(PyramidWriteError::Io)?;
                fs::write(chunk.join(chunk_x.to_string()), bytes).map_err(PyramidWriteError::Io)?;
            }
        }
    }
    Ok(())
}

fn checked_chunk_byte_len(chunk_shape_zyx: [usize; 3]) -> Result<usize, PyramidWriteError> {
    chunk_shape_zyx[0]
        .checked_mul(chunk_shape_zyx[1])
        .and_then(|count| count.checked_mul(chunk_shape_zyx[2]))
        .and_then(|count| count.checked_mul(std::mem::size_of::<u16>()))
        .ok_or(PyramidWriteError::ChunkShapeOverflow(chunk_shape_zyx))
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), PyramidWriteError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(PyramidWriteError::Json)?;
    fs::write(path, bytes).map_err(PyramidWriteError::Io)
}

fn level_shape(base_shape: [u64; 3], factors: [u64; 3]) -> [u64; 3] {
    std::array::from_fn(|axis| base_shape[axis].div_ceil(factors[axis]))
}

#[derive(Clone, Debug, PartialEq)]
pub enum PyramidError {
    CannotReduceFurther,
    FactorOverflow,
    InvalidMaxSize,
    InvalidVoxelSize([f64; 3]),
    ZeroDimension,
}

#[derive(Debug)]
pub enum PyramidWriteError {
    ChunkShapeOverflow([usize; 3]),
    DestinationNotEmpty(PathBuf),
    FactorTooLarge,
    InvalidChunkShape([usize; 3]),
    InvalidVolume,
    InvalidVoxelSize([f64; 3]),
    Io(std::io::Error),
    Json(serde_json::Error),
    Plan(PyramidError),
    VolumeShapeOverflow,
}

impl fmt::Display for PyramidWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChunkShapeOverflow(shape) => write!(
                formatter,
                "pyramid chunk dimensions overflow an addressable byte length: {shape:?}"
            ),
            Self::DestinationNotEmpty(path) => write!(
                formatter,
                "pyramid destination is not empty: {}",
                path.display()
            ),
            Self::FactorTooLarge => write!(formatter, "pyramid factor does not fit this platform"),
            Self::InvalidChunkShape(shape) => write!(
                formatter,
                "pyramid chunk dimensions must be non-zero, got {shape:?}"
            ),
            Self::InvalidVolume => write!(
                formatter,
                "pyramid level-zero dimensions do not match its voxel data"
            ),
            Self::InvalidVoxelSize(size) => {
                write!(formatter, "invalid pyramid voxel size {size:?}")
            }
            Self::Io(error) => write!(formatter, "pyramid IO error: {error}"),
            Self::Json(error) => write!(formatter, "pyramid metadata JSON error: {error}"),
            Self::Plan(error) => write!(formatter, "pyramid planning error: {error}"),
            Self::VolumeShapeOverflow => {
                write!(formatter, "pyramid volume shape overflows this platform")
            }
        }
    }
}

impl std::error::Error for PyramidWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Plan(error) => Some(error),
            Self::ChunkShapeOverflow(_)
            | Self::DestinationNotEmpty(_)
            | Self::FactorTooLarge
            | Self::InvalidChunkShape(_)
            | Self::InvalidVolume
            | Self::InvalidVoxelSize(_)
            | Self::VolumeShapeOverflow => None,
        }
    }
}

impl fmt::Display for PyramidError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CannotReduceFurther => write!(formatter, "pyramid cannot reduce further"),
            Self::FactorOverflow => write!(formatter, "pyramid factor overflow"),
            Self::InvalidMaxSize => write!(formatter, "pyramid max size must be positive"),
            Self::InvalidVoxelSize(size) => write!(formatter, "invalid voxel size {size:?}"),
            Self::ZeroDimension => write!(formatter, "pyramid dimensions must be positive"),
        }
    }
}

impl std::error::Error for PyramidError {}

#[derive(Clone, Debug, PartialEq)]
pub enum TransformError {
    DimensionMismatch { expected: usize, actual: usize },
    DimensionUnknown,
    EmptyCoordinateSystem,
    InvalidAffineShape,
    NoTransformPath { from: String, to: String },
    NonFiniteValue,
    NonInvertible,
    UnknownCoordinateSystem,
    UnknownLevel(usize),
}

impl fmt::Display for TransformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DimensionMismatch { expected, actual } => {
                write!(
                    formatter,
                    "expected {expected} transform dimensions, got {actual}"
                )
            }
            Self::DimensionUnknown => {
                write!(formatter, "coordinate system has no transform dimension")
            }
            Self::EmptyCoordinateSystem => {
                write!(formatter, "coordinate-system name must not be empty")
            }
            Self::InvalidAffineShape => {
                write!(formatter, "affine matrix must be finite and square")
            }
            Self::NoTransformPath { from, to } => {
                write!(formatter, "no transform path from {from} to {to}")
            }
            Self::NonFiniteValue => write!(formatter, "transform values must be finite"),
            Self::NonInvertible => write!(formatter, "affine transform is not invertible"),
            Self::UnknownCoordinateSystem => write!(formatter, "unknown coordinate system"),
            Self::UnknownLevel(level) => write!(formatter, "unknown multiscale level {level}"),
        }
    }
}

impl std::error::Error for TransformError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_transform_composes_local_then_shared_transforms() {
        let multiscale = Multiscale {
            axes: vec![
                Axis {
                    name: "z".into(),
                    kind: None,
                    unit: None,
                },
                Axis {
                    name: "y".into(),
                    kind: None,
                    unit: None,
                },
                Axis {
                    name: "x".into(),
                    kind: None,
                    unit: None,
                },
            ],
            datasets: vec![MultiscaleDataset {
                path: "0".into(),
                coordinate_transformations: vec![
                    CoordinateTransformation::Scale {
                        scale: vec![5.0, 0.5, 0.5],
                    },
                    CoordinateTransformation::Translation {
                        translation: vec![10.0, 20.0, 30.0],
                    },
                ],
            }],
            name: None,
            coordinate_transformations: vec![CoordinateTransformation::Scale {
                scale: vec![2.0, 2.0, 2.0],
            }],
        };

        assert_eq!(
            level_transform(&multiscale, 0)
                .unwrap()
                .apply(&[1.0, 2.0, 3.0])
                .unwrap(),
            [30.0, 42.0, 63.0]
        );
    }

    #[test]
    fn affine_inverse_apply_recovers_source_coordinates_and_rejects_singular_matrices() {
        let transform = AffineTransform::from_matrix(vec![
            vec![2.0, 0.0, 10.0],
            vec![0.0, 0.5, -3.0],
            vec![0.0, 0.0, 1.0],
        ])
        .unwrap();
        assert_eq!(
            transform.inverse_apply(&[18.0, -1.0]).unwrap(),
            vec![4.0, 4.0]
        );
        let singular = AffineTransform::from_matrix(vec![
            vec![1.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0],
            vec![0.0, 0.0, 1.0],
        ])
        .unwrap();
        assert!(matches!(
            singular.inverse_apply(&[1.0, 2.0]),
            Err(TransformError::NonInvertible)
        ));
    }

    #[test]
    fn graph_composes_named_coordinate_systems() {
        let mut graph = TransformGraph::default();
        graph.add_system("voxel").unwrap();
        graph.add_system("stage").unwrap();
        graph.add_system("world").unwrap();
        graph
            .add_edge(TransformEdge {
                from: "voxel".into(),
                to: "stage".into(),
                transform: AffineTransform::from_operations(
                    2,
                    &[CoordinateTransformation::Scale {
                        scale: vec![2.0, 3.0],
                    }],
                )
                .unwrap(),
            })
            .unwrap();
        graph
            .add_edge(TransformEdge {
                from: "stage".into(),
                to: "world".into(),
                transform: AffineTransform::from_operations(
                    2,
                    &[CoordinateTransformation::Translation {
                        translation: vec![10.0, 20.0],
                    }],
                )
                .unwrap(),
            })
            .unwrap();

        assert_eq!(
            graph
                .transform("voxel", "world")
                .unwrap()
                .apply(&[1.0, 2.0])
                .unwrap(),
            [12.0, 26.0]
        );
    }

    #[test]
    fn metadata_parses_standard_ome_ngff_legacy_transforms() {
        let metadata: DatasetMetadata = serde_json::from_str(r#"{
            "multiscales": [{
                "axes": [{"name":"z","type":"space"},{"name":"y","type":"space"},{"name":"x","type":"space"}],
                "datasets": [{"path":"0","coordinateTransformations":[{"type":"scale","scale":[5,0.5,0.5]}]}]
            }],
            "omero": {"channels": [{"active":true,"color":"FFFFFF","window":{"start":1,"end":2,"min":0,"max":3}}]}
        }"#).unwrap();

        assert_eq!(metadata.multiscales[0].axes.len(), 3);
        assert_eq!(
            level_transform(&metadata.multiscales[0], 0)
                .unwrap()
                .apply(&[1.0, 2.0, 3.0])
                .unwrap(),
            [5.0, 1.0, 1.5]
        );
        assert!(metadata.omero.unwrap().channels[0].active);
    }

    #[test]
    fn reads_v3_and_v2_root_metadata() {
        let v3 = tempfile::tempdir().unwrap();
        std::fs::write(
            v3.path().join("zarr.json"),
            r#"{"zarr_format":3,"attributes":{"multiscales":[{"axes":[{"name":"x"}],"datasets":[{"path":"0"}] }]}}"#,
        )
        .unwrap();
        assert_eq!(
            read_dataset_metadata(v3.path()).unwrap().multiscales[0].axes[0].name,
            "x"
        );

        let v2 = tempfile::tempdir().unwrap();
        std::fs::write(
            v2.path().join(".zattrs"),
            r#"{"multiscales":[{"axes":[{"name":"z"}],"datasets":[{"path":"0"}]}]}"#,
        )
        .unwrap();
        assert_eq!(
            read_dataset_metadata(v2.path()).unwrap().multiscales[0].axes[0].name,
            "z"
        );
    }

    #[test]
    fn transport_independent_v3_parsers_preserve_metadata_shape() {
        let root = parse_v3_root_metadata(
            br#"{"zarr_format":3,"attributes":{"multiscales":[{"axes":[{"name":"x"}],"datasets":[{"path":"0"}]}]}}"#,
        )
        .unwrap();
        assert_eq!(root.multiscales[0].datasets[0].path, "0");

        let array = parse_v3_array_info(
            br#"{"zarr_format":3,"node_type":"array","shape":[4,5,6],"data_type":"uint16","chunk_grid":{"configuration":{"chunk_shape":[2,3,4]}}}"#,
        )
        .unwrap();
        assert_eq!(array.shape, [4, 5, 6]);
        assert_eq!(array.chunks, [2, 3, 4]);
        assert_eq!(array.dtype, "uint16");
    }

    #[test]
    fn committed_anisotropic_fixture_keeps_axis_scale_and_translation() {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-data/anisotropic.ome.zarr");
        let metadata = read_dataset_metadata(&root).unwrap();
        let multiscale = &metadata.multiscales[0];

        assert_eq!(
            multiscale
                .axes
                .iter()
                .map(|axis| axis.name.as_str())
                .collect::<Vec<_>>(),
            ["z", "y", "x"]
        );
        assert_eq!(
            metadata.omero.as_ref().unwrap().channels[0]
                .window
                .unwrap()
                .end,
            240.0
        );

        let level_zero = level_transform(multiscale, 0).unwrap();
        assert_eq!(
            level_zero.apply(&[2.0, 4.0, 6.0]).unwrap(),
            [11.25, 4.5, 8.0]
        );

        // Coarser levels preserve the physical Z scale; only X/Y are decimated.
        let level_two = level_transform(multiscale, 2).unwrap();
        assert_eq!(
            level_two.apply(&[2.0, 4.0, 6.0]).unwrap(),
            [11.25, 10.5, 17.0]
        );
    }

    #[test]
    fn cells3d_fixture_has_real_pixels_and_an_anisotropic_ngff_pyramid() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let metadata = read_dataset_metadata(&root).unwrap();
        let multiscale = &metadata.multiscales[0];

        assert_eq!(multiscale.datasets.len(), 3);
        assert_eq!(
            multiscale
                .axes
                .iter()
                .map(|axis| axis.name.as_str())
                .collect::<Vec<_>>(),
            ["z", "y", "x"]
        );
        assert_eq!(
            read_array_info(&root, "0").unwrap(),
            ArrayInfo {
                shape: vec![32, 128, 128],
                chunks: vec![8, 32, 32],
                dtype: "uint16".into(),
                order: None,
                compressor: None,
            }
        );
        assert_eq!(
            level_transform(multiscale, 0)
                .unwrap()
                .apply(&[1.0, 2.0, 3.0])
                .unwrap(),
            [0.29, 0.52, 0.78]
        );
        assert_eq!(
            level_transform(multiscale, 2)
                .unwrap()
                .apply(&[1.0, 2.0, 3.0])
                .unwrap(),
            [0.58, 2.08, 3.12]
        );
    }

    #[test]
    fn cells3d_raw_v3_chunks_assemble_in_zyx_order_under_a_cap() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/cells3d-anisotropic.ome.zarr");
        let registry = SourceRegistry::new(
            LocalSourcePolicy::new(vec![root.parent().unwrap().to_path_buf()]).unwrap(),
            RemoteSourcePolicy::default(),
            MAX_REMOTE_ASSET_BYTES,
        )
        .unwrap();
        let source = registry.open_local(&root).unwrap();
        let volume = read_v3_raw_u16_volume(&source, "0", 2 * 32 * 128 * 128).unwrap();
        assert_eq!(volume.dimensions_zyx, [32, 128, 128]);
        assert_eq!(volume.voxels.len(), 32 * 128 * 128);
        let first_chunk = source.fetch_asset("0/c/0/0/0").unwrap();
        assert_eq!(
            volume.voxels[0],
            u16::from_le_bytes([first_chunk[0], first_chunk[1]])
        );
        let next_z_chunk = source.fetch_asset("0/c/1/0/0").unwrap();
        let z8 = 8 * 128 * 128;
        assert_eq!(
            volume.voxels[z8],
            u16::from_le_bytes([next_z_chunk[0], next_z_chunk[1]])
        );
        assert!(matches!(
            read_v3_raw_u16_volume(&source, "0", 1024),
            Err(RawV3U16VolumeError::OutputTooLarge { .. })
        ));
    }

    #[test]
    fn raw_v3_volume_rejects_a_compressed_codec_before_chunk_reads() {
        let allowed = tempfile::tempdir().unwrap();
        let dataset = allowed.path().join("compressed.ome.zarr");
        let array = dataset.join("0");
        std::fs::create_dir_all(&array).unwrap();
        std::fs::write(
            array.join("zarr.json"),
            r#"{
                "shape":[2,2,2], "data_type":"uint16",
                "chunk_grid":{"name":"regular","configuration":{"chunk_shape":[2,2,2]}},
                "chunk_key_encoding":{"name":"default","configuration":{"separator":"/"}},
                "codecs":[{"name":"zstd","configuration":{}}]
            }"#,
        )
        .unwrap();
        let registry = SourceRegistry::new(
            LocalSourcePolicy::new(vec![allowed.path().to_path_buf()]).unwrap(),
            RemoteSourcePolicy::default(),
            MAX_REMOTE_ASSET_BYTES,
        )
        .unwrap();
        let source = registry.open_local(&dataset).unwrap();
        assert!(matches!(
            read_v3_raw_u16_volume(&source, "0", 1024),
            Err(RawV3U16VolumeError::Layout(message))
                if message.contains("little-endian Zarr v3 bytes codec")
        ));
    }

    #[test]
    fn rejects_oversized_root_metadata_before_json_parsing() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("zarr.json"),
            vec![b' '; MAX_ROOT_METADATA_BYTES as usize + 1],
        )
        .unwrap();

        assert!(matches!(
            read_dataset_metadata(root.path()),
            Err(MetadataError::TooLarge { .. })
        ));
    }

    #[test]
    fn reads_v3_array_shape_and_rejects_path_escape() {
        let root = tempfile::tempdir().unwrap();
        let array = root.path().join("0");
        std::fs::create_dir(&array).unwrap();
        std::fs::write(
            array.join("zarr.json"),
            r#"{"shape":[7,11,13],"data_type":"uint16","chunk_grid":{"configuration":{"chunk_shape":[2,3,5]}}}"#,
        )
        .unwrap();
        assert_eq!(
            read_array_info(root.path(), "0").unwrap(),
            ArrayInfo {
                shape: vec![7, 11, 13],
                chunks: vec![2, 3, 5],
                dtype: "uint16".into(),
                order: None,
                compressor: None
            }
        );
        assert!(matches!(
            read_array_info(root.path(), "../escape"),
            Err(MetadataError::InvalidArrayPath(_))
        ));
    }

    #[test]
    fn local_source_policy_requires_an_explicit_containing_root() {
        let allowed = tempfile::tempdir().unwrap();
        let dataset = allowed.path().join("dataset.ome.zarr");
        std::fs::create_dir(&dataset).unwrap();
        let outside = tempfile::tempdir().unwrap();

        let policy = LocalSourcePolicy::new(vec![allowed.path().to_path_buf()]).unwrap();
        assert_eq!(
            policy.authorize(&dataset).unwrap(),
            dataset.canonicalize().unwrap()
        );
        assert!(matches!(
            policy.authorize(outside.path()),
            Err(SourcePolicyError::NotAllowed(_))
        ));
        assert!(matches!(
            LocalSourcePolicy::default().authorize(&dataset),
            Err(SourcePolicyError::NotAllowed(_))
        ));
    }

    #[test]
    fn source_registry_returns_only_authorized_local_or_https_sources() {
        let allowed = tempfile::tempdir().unwrap();
        let dataset = allowed.path().join("dataset.ome.zarr");
        std::fs::create_dir(&dataset).unwrap();
        std::fs::write(
            dataset.join("zarr.json"),
            r#"{"attributes":{"multiscales":[{"axes":[{"name":"x"}],"datasets":[{"path":"0"}]}]}}"#,
        )
        .unwrap();
        let outside = tempfile::tempdir().unwrap();
        let registry = SourceRegistry::new(
            LocalSourcePolicy::new(vec![allowed.path().to_path_buf()]).unwrap(),
            RemoteSourcePolicy::new(vec!["data.example.org".into()]).unwrap(),
            1024,
        )
        .unwrap();

        let local = registry.open_local(&dataset).unwrap();
        assert!(matches!(local, DatasetSource::Local(_)));
        assert_eq!(local.read_dataset_metadata().unwrap().multiscales.len(), 1);
        assert!(matches!(
            registry.open_local(outside.path()),
            Err(SourceRegistryError::Local(SourcePolicyError::NotAllowed(_)))
        ));
        let remote = registry
            .open_https("https://data.example.org/study/sample.zarr")
            .unwrap();
        assert!(matches!(remote, DatasetSource::Https(_)));
        assert!(matches!(
            registry.open_https("https://other.example.org/study/sample.zarr"),
            Err(SourceRegistryError::Remote(RemoteStoreError::Root(
                RemoteMetadataError::Policy(RemoteSourcePolicyError::NotAllowed(_))
            )))
        ));
        assert!(matches!(
            SourceRegistry::new(
                LocalSourcePolicy::default(),
                RemoteSourcePolicy::default(),
                0
            ),
            Err(SourceRegistryError::InvalidRemoteAssetByteLimit)
        ));
    }

    #[test]
    fn s3_source_policy_scopes_bucket_profile_and_normal_prefix_before_network_io() {
        for invalid_bucket in [
            "bad..bucket",
            "bucket.-label",
            "bucket.label-",
            "192.168.0.1",
        ] {
            assert!(matches!(
                S3SourcePolicy::new(vec![invalid_bucket.into()], Vec::new()),
                Err(S3SourcePolicyError::InvalidBucket(_))
            ));
        }
        let policy = S3SourcePolicy::new(
            vec!["microscopy-data".into()],
            vec!["newvolim-readonly".into()],
        )
        .unwrap();
        let registry = SourceRegistry::new(
            LocalSourcePolicy::default(),
            RemoteSourcePolicy::default(),
            1024,
        )
        .unwrap()
        .with_s3_policy(policy);
        let source = registry
            .open_s3(
                "microscopy-data",
                "/study/cells3d/",
                "newvolim-readonly",
                Some("eu-north-1"),
            )
            .unwrap();
        assert!(matches!(
            source,
            DatasetSource::S3(ref store)
                if store.bucket() == "microscopy-data" && store.prefix() == "study/cells3d"
        ));
        assert!(matches!(
            source.fetch_asset("../escape"),
            Err(DatasetAssetError::S3(S3StoreError::InvalidAssetPath(_)))
        ));
        assert!(matches!(
            registry.open_s3("other-bucket", "study", "newvolim-readonly", None),
            Err(SourceRegistryError::S3(S3StoreError::Policy(
                S3SourcePolicyError::BucketNotAllowed(_)
            )))
        ));
        assert!(matches!(
            registry.open_s3("microscopy-data", "../escape", "newvolim-readonly", None),
            Err(SourceRegistryError::S3(S3StoreError::InvalidPrefix(_)))
        ));
        assert!(matches!(
            registry.open_s3("microscopy-data", "study", "other", None),
            Err(SourceRegistryError::S3(S3StoreError::Policy(
                S3SourcePolicyError::ProfileNotAllowed(_)
            )))
        ));
        assert!(matches!(
            S3SourcePolicy::new(vec!["BAD_BUCKET".into()], vec!["profile".into()]),
            Err(S3SourcePolicyError::InvalidBucket(_))
        ));
    }

    #[test]
    fn authorized_local_sources_expose_only_bounded_normal_assets() {
        let allowed = tempfile::tempdir().unwrap();
        let dataset = allowed.path().join("dataset.ome.zarr");
        let chunk = dataset.join("0/c/0/0");
        std::fs::create_dir_all(chunk.parent().unwrap()).unwrap();
        std::fs::write(&chunk, [1_u8, 2, 3, 4]).unwrap();
        std::fs::write(dataset.join("oversized"), [0_u8; 5]).unwrap();

        let registry = SourceRegistry::new(
            LocalSourcePolicy::new(vec![allowed.path().to_path_buf()]).unwrap(),
            RemoteSourcePolicy::default(),
            1024,
        )
        .unwrap();
        let source = registry.open_local(&dataset).unwrap();
        assert_eq!(source.fetch_asset("0/c/0/0").unwrap(), [1, 2, 3, 4]);
        assert!(matches!(
            source.fetch_asset("../outside"),
            Err(DatasetAssetError::Local(LocalAssetError::InvalidAssetPath(
                _
            )))
        ));
        assert!(matches!(
            read_local_asset(&dataset, "oversized", 4),
            Err(LocalAssetError::TooLarge { limit: 4, .. })
        ));
        assert!(matches!(
            read_local_asset(&dataset, "oversized", 0),
            Err(LocalAssetError::InvalidByteLimit)
        ));
        #[cfg(unix)]
        {
            let outside = allowed.path().join("outside-chunk");
            std::fs::write(&outside, [9_u8]).unwrap();
            std::os::unix::fs::symlink(&outside, dataset.join("symlink-outside")).unwrap();
            assert!(matches!(
                source.fetch_asset("symlink-outside"),
                Err(DatasetAssetError::Local(LocalAssetError::EscapesRoot(_)))
            ));
        }
    }

    #[test]
    fn remote_source_policy_allows_only_credential_free_https_origins() {
        let policy = RemoteSourcePolicy::new(vec!["data.example.org".into()]).unwrap();
        assert_eq!(
            policy
                .authorize("https://data.example.org/study.zarr")
                .unwrap()
                .host_str(),
            Some("data.example.org")
        );
        assert!(matches!(
            policy.authorize("http://data.example.org/study.zarr"),
            Err(RemoteSourcePolicyError::UnsafeUrl(_))
        ));
        assert!(matches!(
            policy.authorize("https://user:secret@data.example.org/study.zarr"),
            Err(RemoteSourcePolicyError::UnsafeUrl(_))
        ));
        assert!(matches!(
            policy.authorize("https://data.example.org:8443/study.zarr"),
            Err(RemoteSourcePolicyError::UnsafeUrl(_))
        ));
        assert!(matches!(
            policy.authorize("https://other.example.org/study.zarr"),
            Err(RemoteSourcePolicyError::NotAllowed(_))
        ));
    }

    #[test]
    fn remote_fetch_rejects_unsafe_url_before_network_io() {
        let policy = RemoteSourcePolicy::new(vec!["data.example.org".into()]).unwrap();
        assert!(matches!(
            fetch_remote_metadata(&policy, "http://data.example.org/metadata"),
            Err(RemoteFetchError::Policy(
                RemoteSourcePolicyError::UnsafeUrl(_)
            ))
        ));
    }

    #[test]
    fn remote_root_urls_resolve_metadata_below_the_requested_store() {
        let policy = RemoteSourcePolicy::new(vec!["data.example.org".into()]).unwrap();
        let root = remote_root_url(&policy, "https://data.example.org/study/sample.zarr").unwrap();
        assert_eq!(root.as_str(), "https://data.example.org/study/sample.zarr/");
        assert_eq!(
            root.join("zarr.json").unwrap().as_str(),
            "https://data.example.org/study/sample.zarr/zarr.json"
        );
        assert!(matches!(
            remote_root_url(&policy, "https://data.example.org/study.zarr?token=secret"),
            Err(RemoteMetadataError::InvalidRootUrl(_))
        ));
    }

    #[test]
    fn remote_zarr_store_derives_only_bounded_normal_asset_urls() {
        let policy = RemoteSourcePolicy::new(vec!["data.example.org".into()]).unwrap();
        let store =
            RemoteZarrStore::new(policy, "https://data.example.org/study/sample.zarr", 1024)
                .unwrap();
        assert_eq!(
            store.asset_url("0/c/1/2/3").unwrap().as_str(),
            "https://data.example.org/study/sample.zarr/0/c/1/2/3"
        );
        for unsafe_asset in ["", "../secret", "/etc/passwd", "0/%2e%2e/secret", "0/a?b"] {
            assert!(matches!(
                store.asset_url(unsafe_asset),
                Err(RemoteStoreError::InvalidAssetPath(_))
            ));
        }
        assert!(matches!(
            RemoteZarrStore::new(
                RemoteSourcePolicy::new(vec!["data.example.org".into()]).unwrap(),
                "https://data.example.org/study/sample.zarr",
                0,
            ),
            Err(RemoteStoreError::InvalidByteLimit)
        ));
        assert!(matches!(
            fetch_remote_bytes(
                &RemoteSourcePolicy::new(vec!["data.example.org".into()]).unwrap(),
                "http://data.example.org/chunk",
                1024,
            ),
            Err(RemoteFetchError::Policy(
                RemoteSourcePolicyError::UnsafeUrl(_)
            ))
        ));
    }

    #[test]
    fn remote_zarr_array_metadata_rejects_escape_before_fetching() {
        let policy = RemoteSourcePolicy::new(vec!["data.example.org".into()]).unwrap();
        let store = RemoteZarrStore::new(
            policy,
            "https://data.example.org/study/sample.zarr",
            MAX_REMOTE_ASSET_BYTES,
        )
        .unwrap();

        assert!(matches!(
            store.read_array_info("../outside"),
            Err(RemoteStoreError::InvalidAssetPath(_))
        ));
        assert!(matches!(
            store.read_array_info(""),
            Err(RemoteStoreError::InvalidAssetPath(_))
        ));
    }

    #[test]
    fn pyramid_delays_z_for_ten_to_one_voxels() {
        let levels = propose_pyramid([65_536, 65_536, 512], [1.0, 1.0, 10.0], 256).unwrap();
        let factors: Vec<_> = levels.iter().map(|level| level.factors).collect();

        assert_eq!(
            &factors[..6],
            &[
                [1, 1, 1],
                [2, 2, 1],
                [4, 4, 1],
                [8, 8, 1],
                [16, 16, 2],
                [32, 32, 4],
            ]
        );
        assert!(levels.last().unwrap().shape.iter().all(|&size| size <= 256));
    }

    #[test]
    fn pyramid_rejects_non_physical_input() {
        assert_eq!(
            propose_pyramid([8, 8, 8], [1.0, 0.0, 1.0], 4),
            Err(PyramidError::InvalidVoxelSize([1.0, 0.0, 1.0]))
        );
        assert_eq!(
            propose_pyramid([8, 8, 0], [1.0, 1.0, 1.0], 4),
            Err(PyramidError::ZeroDimension)
        );
    }

    #[test]
    fn raw_v3_pyramid_writer_round_trips_anisotropic_levels_and_edge_chunks() {
        let temporary = tempfile::tempdir().unwrap();
        let output = temporary.path().join("generated.ome.zarr");
        let level_zero = RawV3U16Volume {
            dimensions_zyx: [2, 2, 4],
            voxels: (1_u16..=16).collect(),
        };
        let levels =
            write_v3_raw_u16_pyramid(&output, &level_zero, [10.0, 1.0, 1.0], [1, 2, 2], 2).unwrap();
        assert_eq!(
            levels.iter().map(|level| level.factors).collect::<Vec<_>>(),
            vec![[1, 1, 1], [2, 2, 1]]
        );
        let metadata = read_dataset_metadata(&output).unwrap();
        assert_eq!(metadata.multiscales[0].datasets.len(), 2);
        assert_eq!(
            level_transform(&metadata.multiscales[0], 1)
                .unwrap()
                .apply(&[1.0, 1.0, 1.0])
                .unwrap(),
            vec![10.0, 2.0, 2.0]
        );
        let registry = SourceRegistry::new(
            LocalSourcePolicy::new(vec![temporary.path().to_path_buf()]).unwrap(),
            RemoteSourcePolicy::default(),
            MAX_REMOTE_ASSET_BYTES,
        )
        .unwrap();
        let source = registry.open_local(&output).unwrap();
        assert_eq!(
            read_v3_raw_u16_volume(&source, "0", 1024).unwrap(),
            level_zero
        );
        assert_eq!(
            read_v3_raw_u16_volume(&source, "1", 1024).unwrap(),
            RawV3U16Volume {
                dimensions_zyx: [2, 1, 2],
                voxels: vec![4, 6, 12, 14],
            }
        );
        assert!(matches!(
            write_v3_raw_u16_pyramid(&output, &level_zero, [10.0, 1.0, 1.0], [1, 2, 2], 2),
            Err(PyramidWriteError::DestinationNotEmpty(_))
        ));
        let overflowing = temporary.path().join("overflow.ome.zarr");
        assert!(matches!(
            write_v3_raw_u16_pyramid(
                &overflowing,
                &level_zero,
                [10.0, 1.0, 1.0],
                [usize::MAX, 2, 1],
                2,
            ),
            Err(PyramidWriteError::ChunkShapeOverflow(_))
        ));
        assert!(!overflowing.exists());
    }
}
