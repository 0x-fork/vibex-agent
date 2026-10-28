use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{AppUpdateError, AppUpdateResult};

pub const CURRENT_UPDATER_VERSION: u32 = 1;
const MANIFEST_SCHEMA_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: usize = 512 * 1024;
const MAX_ARTIFACTS: usize = 32;
const MAX_ARTIFACT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const OFFICIAL_REPOSITORY_HOST: &str = "github.com";
const OFFICIAL_REPOSITORY_PATH: &str = "/vibex-ai/vibex";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    Stable,
    Rc,
    Preview,
}

impl UpdateChannel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Rc => "rc",
            Self::Preview => "preview",
        }
    }

    pub fn accepts(self, version: &Version) -> bool {
        match self {
            Self::Stable => version.pre.is_empty(),
            Self::Rc => version
                .pre
                .as_str()
                .split('.')
                .next()
                .is_some_and(|identifier| identifier == "rc"),
            Self::Preview => version
                .pre
                .as_str()
                .split('.')
                .next()
                .is_some_and(|identifier| identifier == "preview"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallMode {
    SelfReplace,
    SystemInstaller,
    Store,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateArtifact {
    pub os: String,
    pub arch: String,
    pub package: String,
    pub install_mode: InstallMode,
    pub url: Url,
    pub size: u64,
    pub sha256: String,
    #[serde(default)]
    pub sha512: Option<String>,
}

/// A headless runtime artifact the desktop installs over SSH.
///
/// Server artifacts are not desktop packages: they carry no install mode and
/// are selected by OS/architecture alone. The release tag and the signed
/// manifest bind them to the same release as the desktop packages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerArtifact {
    pub os: String,
    pub arch: String,
    pub url: Url,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateManifest {
    pub schema: u32,
    pub channel: UpdateChannel,
    pub version: Version,
    pub tag: String,
    pub published_at: String,
    pub minimum_updater_version: String,
    pub notes_url: Url,
    pub artifacts: Vec<UpdateArtifact>,
    /// Optional so older manifests stay readable and older clients ignore the
    /// field they do not know.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub server_artifacts: Vec<ServerArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedManifest {
    pub manifest: UpdateManifest,
    pub raw_sha256: String,
}

impl VerifiedManifest {
    pub fn matching_artifact(
        &self,
        os: &str,
        arch: &str,
        package: &str,
    ) -> Option<&UpdateArtifact> {
        self.manifest.artifacts.iter().find(|artifact| {
            artifact.os == os && artifact.arch == arch && artifact.package == package
        })
    }

    /// The headless runtime artifact for one target, if this release ships it.
    pub fn server_artifact(&self, os: &str, arch: &str) -> Option<&ServerArtifact> {
        self.manifest
            .server_artifacts
            .iter()
            .find(|artifact| artifact.os == os && artifact.arch == arch)
    }
}

pub fn verify_manifest(
    raw: &[u8],
    signature_base64: &str,
    public_key_base64: &str,
    expected_channel: UpdateChannel,
    expected_tag: &str,
) -> AppUpdateResult<VerifiedManifest> {
    if raw.is_empty() || raw.len() > MAX_MANIFEST_BYTES {
        return Err(AppUpdateError::new(
            "app_update_manifest_size_invalid",
            "the update manifest size is invalid",
        ));
    }
    let public_key = decode_fixed_base64::<32>(
        public_key_base64,
        "app_update_public_key_invalid",
        "the embedded update verification key is invalid",
    )?;
    let signature = decode_fixed_base64::<64>(
        signature_base64.trim(),
        "app_update_signature_invalid",
        "the update manifest signature is invalid",
    )?;
    let verifying_key = VerifyingKey::from_bytes(&public_key).map_err(|_| {
        AppUpdateError::new(
            "app_update_public_key_invalid",
            "the embedded update verification key is invalid",
        )
    })?;
    verifying_key
        .verify_strict(raw, &Signature::from_bytes(&signature))
        .map_err(|_| {
            AppUpdateError::new(
                "app_update_signature_invalid",
                "the update manifest signature could not be verified",
            )
        })?;

    let manifest: UpdateManifest = serde_json::from_slice(raw).map_err(|_| {
        AppUpdateError::new(
            "app_update_manifest_invalid",
            "the signed update manifest is invalid",
        )
    })?;
    validate_manifest(&manifest, expected_channel, expected_tag)?;
    Ok(VerifiedManifest {
        manifest,
        raw_sha256: sha256_hex(raw),
    })
}

fn validate_manifest(
    manifest: &UpdateManifest,
    expected_channel: UpdateChannel,
    expected_tag: &str,
) -> AppUpdateResult<()> {
    if manifest.schema != MANIFEST_SCHEMA_VERSION {
        return Err(AppUpdateError::new(
            "app_update_manifest_schema_unsupported",
            "the update manifest schema is not supported",
        ));
    }
    if manifest.channel != expected_channel || !expected_channel.accepts(&manifest.version) {
        return Err(AppUpdateError::new(
            "app_update_channel_mismatch",
            "the update belongs to a different release channel",
        ));
    }
    let required_tag = format!("v{}", manifest.version);
    if manifest.tag != expected_tag || manifest.tag != required_tag {
        return Err(AppUpdateError::new(
            "app_update_release_identity_mismatch",
            "the update release identity is inconsistent",
        ));
    }
    let minimum_updater_version =
        manifest
            .minimum_updater_version
            .parse::<u32>()
            .map_err(|_| {
                AppUpdateError::new(
                    "app_update_minimum_version_invalid",
                    "the update manifest requires an invalid updater version",
                )
            })?;
    if minimum_updater_version > CURRENT_UPDATER_VERSION {
        return Err(AppUpdateError::new(
            "app_update_updater_too_old",
            "this Vibex version cannot safely install the published update",
        ));
    }
    validate_release_url(&manifest.notes_url, &manifest.tag, false)?;
    if manifest.published_at.trim().is_empty() || manifest.published_at.len() > 80 {
        return Err(AppUpdateError::new(
            "app_update_published_at_invalid",
            "the update publication time is invalid",
        ));
    }
    if manifest.artifacts.is_empty() || manifest.artifacts.len() > MAX_ARTIFACTS {
        return Err(AppUpdateError::new(
            "app_update_artifact_set_invalid",
            "the update artifact set is invalid",
        ));
    }
    for artifact in &manifest.artifacts {
        validate_artifact(artifact, &manifest.tag)?;
    }
    if manifest.server_artifacts.len() > MAX_ARTIFACTS {
        return Err(AppUpdateError::new(
            "app_update_server_artifact_set_invalid",
            "the headless runtime artifact set is invalid",
        ));
    }
    let mut server_targets = std::collections::BTreeSet::new();
    for artifact in &manifest.server_artifacts {
        validate_server_artifact(artifact, &manifest.tag)?;
        if !server_targets.insert((artifact.os.as_str(), artifact.arch.as_str())) {
            return Err(AppUpdateError::new(
                "app_update_server_artifact_duplicate",
                "the update manifest contains a duplicate headless runtime target",
            ));
        }
    }
    Ok(())
}

fn validate_server_artifact(artifact: &ServerArtifact, tag: &str) -> AppUpdateResult<()> {
    if !matches!(artifact.os.as_str(), "linux" | "macos") {
        return Err(AppUpdateError::new(
            "app_update_server_artifact_target_invalid",
            "a headless runtime artifact target is invalid",
        ));
    }
    if !matches!(artifact.arch.as_str(), "x86_64" | "arm64") {
        return Err(AppUpdateError::new(
            "app_update_server_artifact_target_invalid",
            "a headless runtime artifact architecture is invalid",
        ));
    }
    validate_target_token(&artifact.os)?;
    validate_target_token(&artifact.arch)?;
    if artifact.size == 0 || artifact.size > MAX_ARTIFACT_BYTES {
        return Err(AppUpdateError::new(
            "app_update_server_artifact_size_invalid",
            "a headless runtime artifact size is invalid",
        ));
    }
    if !valid_hex_digest(&artifact.sha256, 64) {
        return Err(AppUpdateError::new(
            "app_update_server_artifact_digest_invalid",
            "a headless runtime artifact digest is invalid",
        ));
    }
    validate_release_url(&artifact.url, tag, true)
}

fn validate_target_token(token: &str) -> AppUpdateResult<()> {
    if token.is_empty()
        || token.len() > 40
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(AppUpdateError::new(
            "app_update_artifact_target_invalid",
            "an update artifact target is invalid",
        ));
    }
    Ok(())
}

fn validate_artifact(artifact: &UpdateArtifact, tag: &str) -> AppUpdateResult<()> {
    for token in [&artifact.os, &artifact.arch, &artifact.package] {
        validate_target_token(token)?;
    }
    if artifact.size == 0 || artifact.size > MAX_ARTIFACT_BYTES {
        return Err(AppUpdateError::new(
            "app_update_artifact_size_invalid",
            "an update artifact size is invalid",
        ));
    }
    if !valid_hex_digest(&artifact.sha256, 64)
        || artifact
            .sha512
            .as_deref()
            .is_some_and(|digest| !valid_hex_digest(digest, 128))
    {
        return Err(AppUpdateError::new(
            "app_update_artifact_digest_invalid",
            "an update artifact digest is invalid",
        ));
    }
    validate_release_url(&artifact.url, tag, true)
}

fn validate_release_url(url: &Url, tag: &str, asset: bool) -> AppUpdateResult<()> {
    let expected_prefix = if asset {
        format!("{OFFICIAL_REPOSITORY_PATH}/releases/download/{tag}/")
    } else {
        format!("{OFFICIAL_REPOSITORY_PATH}/releases/tag/{tag}")
    };
    let valid = url.scheme() == "https"
        && url.host_str() == Some(OFFICIAL_REPOSITORY_HOST)
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && if asset {
            url.path().starts_with(&expected_prefix)
                && url.path().len() > expected_prefix.len()
                && !url.path()[expected_prefix.len()..].contains('/')
        } else {
            url.path() == expected_prefix
        };
    if !valid {
        return Err(AppUpdateError::new(
            "app_update_release_url_invalid",
            "the update manifest contains an invalid release URL",
        ));
    }
    Ok(())
}

fn decode_fixed_base64<const N: usize>(
    encoded: &str,
    code: &'static str,
    message: &'static str,
) -> AppUpdateResult<[u8; N]> {
    let decoded = BASE64_STANDARD
        .decode(encoded.trim())
        .map_err(|_| AppUpdateError::new(code, message))?;
    decoded
        .try_into()
        .map_err(|_| AppUpdateError::new(code, message))
}

fn valid_hex_digest(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex_lower(&Sha256::digest(bytes))
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, SigningKey};
    use serde_json::json;

    use super::*;

    fn signed_manifest(mut value: serde_json::Value) -> (Vec<u8>, String, String) {
        value["published_at"] = json!("2026-08-16T00:00:00Z");
        let raw = serde_json::to_vec(&value).unwrap();
        let key = SigningKey::from_bytes(&[7; 32]);
        let signature = key.sign(&raw);
        (
            raw,
            BASE64_STANDARD.encode(signature.to_bytes()),
            BASE64_STANDARD.encode(key.verifying_key().to_bytes()),
        )
    }

    fn manifest() -> serde_json::Value {
        json!({
            "schema": 1,
            "channel": "stable",
            "version": "0.2.0",
            "tag": "v0.2.0",
            "minimum_updater_version": "1",
            "notes_url": "https://github.com/vibex-ai/vibex/releases/tag/v0.2.0",
            "artifacts": [{
                "os": "linux",
                "arch": "x86_64",
                "package": "appimage",
                "install_mode": "self_replace",
                "url": "https://github.com/vibex-ai/vibex/releases/download/v0.2.0/vibex-0.2.0-linux-x86_64-appimage.AppImage",
                "size": 3,
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }, {
                "os": "android",
                "arch": "arm64",
                "package": "apk",
                "install_mode": "system_installer",
                "url": "https://github.com/vibex-ai/vibex/releases/download/v0.2.0/vibex-0.2.0-android-arm64.apk",
                "size": 3,
                "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            }]
        })
    }

    #[test]
    fn verifies_signature_and_target_selection() {
        let (raw, signature, public_key) = signed_manifest(manifest());
        let verified = verify_manifest(
            &raw,
            &signature,
            &public_key,
            UpdateChannel::Stable,
            "v0.2.0",
        )
        .unwrap();
        assert_eq!(verified.manifest.version, Version::new(0, 2, 0));
        assert!(
            verified
                .matching_artifact("linux", "x86_64", "appimage")
                .is_some()
        );
        // The Android APK is a directly installed package the system installer
        // replaces, so one signed manifest serves both product clients.
        let android = verified
            .matching_artifact("android", "arm64", "apk")
            .expect("the Android APK artifact is selected");
        assert_eq!(android.install_mode, InstallMode::SystemInstaller);
    }

    #[test]
    fn rejects_modified_cross_channel_and_external_manifests() {
        let (mut raw, signature, public_key) = signed_manifest(manifest());
        raw.push(b' ');
        assert_eq!(
            verify_manifest(
                &raw,
                &signature,
                &public_key,
                UpdateChannel::Stable,
                "v0.2.0"
            )
            .unwrap_err()
            .code,
            "app_update_signature_invalid"
        );

        let mut cross_channel = manifest();
        cross_channel["channel"] = json!("preview");
        let (raw, signature, public_key) = signed_manifest(cross_channel);
        assert_eq!(
            verify_manifest(
                &raw,
                &signature,
                &public_key,
                UpdateChannel::Stable,
                "v0.2.0"
            )
            .unwrap_err()
            .code,
            "app_update_channel_mismatch"
        );

        let mut external = manifest();
        external["artifacts"][0]["url"] = json!("https://example.com/update.AppImage");
        let (raw, signature, public_key) = signed_manifest(external);
        assert_eq!(
            verify_manifest(
                &raw,
                &signature,
                &public_key,
                UpdateChannel::Stable,
                "v0.2.0"
            )
            .unwrap_err()
            .code,
            "app_update_release_url_invalid"
        );
    }

    fn manifest_with_server_artifacts() -> serde_json::Value {
        let mut value = manifest();
        value["server_artifacts"] = json!([
            {
                "os": "linux",
                "arch": "x86_64",
                "url": "https://github.com/vibex-ai/vibex/releases/download/v0.2.0/vibex-server-0.2.0-linux-x86_64.tar.gz",
                "size": 4,
                "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
            },
            {
                "os": "linux",
                "arch": "arm64",
                "url": "https://github.com/vibex-ai/vibex/releases/download/v0.2.0/vibex-server-0.2.0-linux-aarch64.tar.gz",
                "size": 4,
                "sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
            }
        ]);
        value
    }

    #[test]
    fn selects_server_artifacts_without_touching_desktop_packages() {
        let (raw, signature, public_key) = signed_manifest(manifest_with_server_artifacts());
        let verified = verify_manifest(
            &raw,
            &signature,
            &public_key,
            UpdateChannel::Stable,
            "v0.2.0",
        )
        .unwrap();
        assert_eq!(verified.manifest.artifacts.len(), 2);
        let server = verified
            .server_artifact("linux", "x86_64")
            .expect("the Linux server artifact is selected");
        assert_eq!(server.size, 4);
        assert!(verified.server_artifact("macos", "arm64").is_none());

        // An older manifest without the optional field still verifies and
        // simply exposes no headless runtime artifact.
        let (raw, signature, public_key) = signed_manifest(manifest());
        let verified = verify_manifest(
            &raw,
            &signature,
            &public_key,
            UpdateChannel::Stable,
            "v0.2.0",
        )
        .unwrap();
        assert!(verified.manifest.server_artifacts.is_empty());
        assert!(verified.server_artifact("linux", "x86_64").is_none());
    }

    #[test]
    fn rejects_invalid_server_artifacts() {
        let mut wrong_tag = manifest_with_server_artifacts();
        wrong_tag["server_artifacts"][0]["url"] = json!(
            "https://github.com/vibex-ai/vibex/releases/download/v0.1.0/vibex-server-0.2.0-linux-x86_64.tar.gz"
        );
        let (raw, signature, public_key) = signed_manifest(wrong_tag);
        assert_eq!(
            verify_manifest(
                &raw,
                &signature,
                &public_key,
                UpdateChannel::Stable,
                "v0.2.0"
            )
            .unwrap_err()
            .code,
            "app_update_release_url_invalid"
        );

        let mut duplicate = manifest_with_server_artifacts();
        duplicate["server_artifacts"][1]["arch"] = json!("x86_64");
        let (raw, signature, public_key) = signed_manifest(duplicate);
        assert_eq!(
            verify_manifest(
                &raw,
                &signature,
                &public_key,
                UpdateChannel::Stable,
                "v0.2.0"
            )
            .unwrap_err()
            .code,
            "app_update_server_artifact_duplicate"
        );

        let mut bad_arch = manifest_with_server_artifacts();
        bad_arch["server_artifacts"][0]["arch"] = json!("aarch64");
        let (raw, signature, public_key) = signed_manifest(bad_arch);
        assert_eq!(
            verify_manifest(
                &raw,
                &signature,
                &public_key,
                UpdateChannel::Stable,
                "v0.2.0"
            )
            .unwrap_err()
            .code,
            "app_update_server_artifact_target_invalid"
        );
    }
}
