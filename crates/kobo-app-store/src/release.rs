use crate::{verify, DetachedSignature, Ed25519PublicKey, PUBLIC_RELEASE_KEY_HEX};

const MAX_RELEASE_MANIFEST_BYTES: usize = 64 * 1024;

/// Where a build's platform updates come from, and who vouches for them.
///
/// A stock build reads the public releases and believes the release key. A
/// build made with `COBALT_RELEASE_REPOSITORY` and `COBALT_PLATFORM_KEY` set
/// is an owner's own: its reader takes platform updates only from that GitHub
/// repository's releases, signed with that key, and never a stock release,
/// which would replace the owner's runtime with one that lacks their changes.
/// The application catalog is not affected: the public Store still answers to
/// the release key in every build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlatformSource {
    /// `owner/repository` on GitHub.
    pub repository: &'static str,
    /// The Ed25519 public key platform manifests are signed with.
    pub key_hex: &'static str,
    /// Whether this is an owner's build rather than a stock one.
    pub owner: bool,
}

/// This build's platform source, fixed when it was compiled.
pub const PLATFORM: PlatformSource = match (
    option_env!("COBALT_RELEASE_REPOSITORY"),
    option_env!("COBALT_PLATFORM_KEY"),
) {
    (Some(repository), Some(key_hex)) => PlatformSource {
        repository,
        key_hex,
        owner: true,
    },
    (None, None) => PlatformSource {
        repository: "BandarLabs/Cobalt",
        key_hex: PUBLIC_RELEASE_KEY_HEX,
        owner: false,
    },
    _ => panic!("set COBALT_RELEASE_REPOSITORY and COBALT_PLATFORM_KEY together, or neither"),
};

/// The signed metadata an owner's release carries: one device package and
/// nothing for a computer, which a stock release's manifest must also name.
pub const DEVICE_MANIFEST_FILE: &str = "cobalt-device-manifest.txt";
const HOST_MANIFEST_FILE: &str = "cobalt-host-manifest.txt";

impl PlatformSource {
    /// The newest stable release, as GitHub describes it.
    #[must_use]
    pub fn stable_releases_url(&self) -> String {
        format!(
            "https://api.github.com/repos/{}/releases/latest",
            self.repository
        )
    }

    /// The newest `count` releases of any kind.
    #[must_use]
    pub fn recent_releases_url(&self, count: u32) -> String {
        format!(
            "https://api.github.com/repos/{}/releases?per_page={count}",
            self.repository
        )
    }

    /// The release asset holding the signed metadata. Its signature is the
    /// asset of the same name with `.sig` after it.
    #[must_use]
    pub const fn manifest_file(&self) -> &'static str {
        if self.owner {
            DEVICE_MANIFEST_FILE
        } else {
            HOST_MANIFEST_FILE
        }
    }

    /// Verifies a release's signed metadata for this build.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed signature or key, a signature this
    /// build's key did not make, or an invalid manifest.
    pub fn verify_manifest(
        &self,
        bytes: &[u8],
        signature_hex: &str,
    ) -> Result<ReleaseManifest, String> {
        if !self.owner {
            return verify_release_manifest(bytes, signature_hex);
        }
        if self.key_hex == PUBLIC_RELEASE_KEY_HEX {
            return Err("an owner build needs a platform key of its own".to_owned());
        }
        let key = Ed25519PublicKey::from_hex(self.key_hex)
            .map_err(|error| format!("invalid platform key: {error}"))?;
        verify_device_manifest(bytes, signature_hex, &key)
    }
}

/// Verifies and parses an owner's device-only release manifest under `key`.
///
/// # Errors
///
/// Returns an error for a malformed signature, verification failure, or an
/// invalid manifest.
pub fn verify_device_manifest(
    bytes: &[u8],
    signature_hex: &str,
    key: &Ed25519PublicKey,
) -> Result<ReleaseManifest, String> {
    let signature = DetachedSignature::from_hex(signature_hex.trim())
        .map_err(|error| format!("invalid release signature: {error}"))?;
    verify(bytes, &signature, key)
        .map_err(|_| "release manifest signature verification failed".to_owned())?;
    ReleaseManifest::parse_device(bytes)
}

/// The canonical text of a device-only release manifest.
///
/// # Errors
///
/// Returns a description of the first field [`ReleaseManifest::parse_device`]
/// would refuse.
pub fn device_manifest(
    version: &str,
    source: &str,
    package: &str,
    bytes: u64,
    sha256: &str,
) -> Result<String, String> {
    let text = format!(
        "cobalt-device-release 1\nversion {version}\nsource {source}\ndevice {package} {bytes} {sha256}\n"
    );
    ReleaseManifest::parse_device(text.as_bytes())?;
    Ok(text)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseAsset {
    pub kind: String,
    pub platform: Option<String>,
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseManifest {
    pub version: String,
    pub channels: Vec<String>,
    pub source: String,
    pub assets: Vec<ReleaseAsset>,
}

impl ReleaseManifest {
    /// Parses an owner's device-only release manifest: a version, the commit
    /// it was built from, and exactly one device package.
    ///
    /// # Errors
    ///
    /// Returns a description of the first malformed or missing field.
    pub fn parse_device(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_RELEASE_MANIFEST_BYTES {
            return Err("release manifest is too large".to_owned());
        }
        let text =
            std::str::from_utf8(bytes).map_err(|_| "release manifest is not UTF-8".to_owned())?;
        if !text.ends_with('\n') {
            return Err("release manifest must end with a newline".to_owned());
        }
        let mut lines = text.lines();
        if lines.next() != Some("cobalt-device-release 1") {
            return Err("unsupported release manifest format".to_owned());
        }
        let version = field(lines.next(), "version")?;
        if !valid_version(&version) {
            return Err("release manifest has an invalid version".to_owned());
        }
        let source = field(lines.next(), "source")?;
        if source.len() != 40 || !source.bytes().all(is_lower_hex) {
            return Err("release manifest has an invalid source commit".to_owned());
        }
        let line = lines
            .next()
            .ok_or_else(|| "release manifest names no device package".to_owned())?;
        let asset = match line.split(' ').collect::<Vec<_>>().as_slice() {
            ["device", name, byte_text, sha256]
                if safe_token(name) && sha256.len() == 64 && sha256.bytes().all(is_lower_hex) =>
            {
                let bytes = byte_text
                    .parse::<u64>()
                    .ok()
                    .filter(|bytes| *bytes > 0)
                    .ok_or_else(|| format!("invalid release asset size in {line:?}"))?;
                ReleaseAsset {
                    kind: "device".to_owned(),
                    platform: None,
                    name: (*name).to_owned(),
                    bytes,
                    sha256: (*sha256).to_owned(),
                }
            }
            _ => return Err(format!("invalid release manifest line {line:?}")),
        };
        if lines.next().is_some() {
            return Err("release manifest must name exactly one device package".to_owned());
        }
        Ok(Self {
            version,
            channels: vec!["stable".to_owned(), "beta".to_owned()],
            source,
            assets: vec![asset],
        })
    }

    /// Parses the canonical host/platform release manifest.
    ///
    /// # Errors
    ///
    /// Returns a description of the first malformed or missing field.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_RELEASE_MANIFEST_BYTES {
            return Err("release manifest is too large".to_owned());
        }
        let text =
            std::str::from_utf8(bytes).map_err(|_| "release manifest is not UTF-8".to_owned())?;
        if !text.ends_with('\n') {
            return Err("release manifest must end with a newline".to_owned());
        }
        let mut lines = text.lines();
        if lines.next() != Some("cobalt-host-release 1") {
            return Err("unsupported release manifest format".to_owned());
        }
        let version = field(lines.next(), "version")?;
        if !valid_version(&version) {
            return Err("release manifest has an invalid version".to_owned());
        }
        let channel_text = lines
            .next()
            .and_then(|line| line.strip_prefix("channels "))
            .ok_or_else(|| "release manifest has no channels".to_owned())?
            .to_owned();
        let channels = channel_text
            .split(',')
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if channels != ["stable", "beta"] {
            return Err(
                "release manifest must be promotable unchanged from beta to stable".to_owned(),
            );
        }
        let source = field(lines.next(), "source")?;
        if source.len() != 40 || !source.bytes().all(is_lower_hex) {
            return Err("release manifest has an invalid source commit".to_owned());
        }

        let mut assets = Vec::new();
        for line in lines {
            let fields = line.split(' ').collect::<Vec<_>>();
            let (kind, platform, name, byte_text, sha256) = match fields.as_slice() {
                ["device", name, bytes, sha256] => ("device", None, *name, *bytes, *sha256),
                ["bootstrap", name, bytes, sha256] => ("bootstrap", None, *name, *bytes, *sha256),
                ["host", platform, name, bytes, sha256] => {
                    ("host", Some((*platform).to_owned()), *name, *bytes, *sha256)
                }
                _ => return Err(format!("invalid release manifest line {line:?}")),
            };
            if !safe_token(name) || sha256.len() != 64 || !sha256.bytes().all(is_lower_hex) {
                return Err(format!("invalid release asset line {line:?}"));
            }
            let bytes = byte_text
                .parse::<u64>()
                .map_err(|_| format!("invalid release asset size in {line:?}"))?;
            if bytes == 0 {
                return Err(format!("empty release asset in {line:?}"));
            }
            assets.push(ReleaseAsset {
                kind: kind.to_owned(),
                platform,
                name: name.to_owned(),
                bytes,
                sha256: sha256.to_owned(),
            });
        }
        if assets.iter().filter(|asset| asset.kind == "device").count() != 1 {
            return Err("release manifest must name exactly one device package".to_owned());
        }
        if assets
            .iter()
            .filter(|asset| asset.kind == "bootstrap" && asset.name == "install.sh")
            .count()
            != 1
        {
            return Err("release manifest must name exactly one install.sh bootstrap".to_owned());
        }
        for platform in ["macos-x86_64", "macos-arm64", "linux-x86_64", "linux-arm64"] {
            if assets
                .iter()
                .filter(|asset| asset.platform.as_deref() == Some(platform))
                .count()
                != 1
            {
                return Err(format!(
                    "release manifest must name exactly one {platform} host package"
                ));
            }
        }
        Ok(Self {
            version,
            channels,
            source,
            assets,
        })
    }

    #[must_use]
    pub fn device(&self) -> Option<&ReleaseAsset> {
        self.assets.iter().find(|asset| asset.kind == "device")
    }

    #[must_use]
    pub fn allows_channel(&self, channel: &str) -> bool {
        self.channels.iter().any(|allowed| allowed == channel)
    }
}

/// Verifies and parses a raw detached Ed25519 release-manifest signature.
///
/// # Errors
///
/// Returns an error for malformed signatures, verification failure, or an
/// invalid manifest.
pub fn verify_release_manifest(
    bytes: &[u8],
    signature_hex: &str,
) -> Result<ReleaseManifest, String> {
    let signature = DetachedSignature::from_hex(signature_hex.trim())
        .map_err(|error| format!("invalid release signature: {error}"))?;
    let public = Ed25519PublicKey::from_hex(PUBLIC_RELEASE_KEY_HEX)
        .map_err(|error| format!("invalid built-in release key: {error}"))?;
    verify(bytes, &signature, &public)
        .map_err(|_| "release manifest signature verification failed".to_owned())?;
    ReleaseManifest::parse(bytes)
}

fn field(line: Option<&str>, name: &str) -> Result<String, String> {
    let line = line.ok_or_else(|| format!("release manifest has no {name}"))?;
    let prefix = format!("{name} ");
    let value = line
        .strip_prefix(&prefix)
        .ok_or_else(|| format!("release manifest has no {name}"))?;
    if !safe_token(value) {
        return Err(format!("release manifest has an invalid {name}"));
    }
    Ok(value.to_owned())
}

fn valid_version(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn safe_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Vec<u8> {
        b"cobalt-host-release 1\nversion 0.3.4\nchannels stable,beta\nsource 0123456789abcdef0123456789abcdef01234567\ndevice cobalt-0.3.4-KoboRoot.tgz 12 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nbootstrap install.sh 13 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\nhost macos-x86_64 kobo-0.3.4-macos-x86_64.tar.gz 14 cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\nhost macos-arm64 kobo-0.3.4-macos-arm64.tar.gz 15 dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd\nhost linux-x86_64 kobo-0.3.4-linux-x86_64.tar.gz 16 eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\nhost linux-arm64 kobo-0.3.4-linux-arm64.tar.gz 17 ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\n".to_vec()
    }

    #[test]
    fn a_device_manifest_round_trips_and_answers_only_to_its_key() {
        let seed = [5_u8; 32];
        let key = crate::derive_public_key(&seed).expect("key");
        let source = "0123456789abcdef0123456789abcdef01234567";
        let text = device_manifest(
            "0.3.3701",
            source,
            "cobalt-0.3.3701-KoboRoot.tgz",
            12,
            &"a".repeat(64),
        )
        .expect("manifest");
        let signature = crate::sign(text.as_bytes(), &seed)
            .expect("signature")
            .to_hex();
        let parsed = verify_device_manifest(text.as_bytes(), &signature, &key).expect("verified");
        assert_eq!(parsed.version, "0.3.3701");
        assert_eq!(parsed.device().expect("device").sha256, "a".repeat(64));

        // Another key, the release key's own check, and a changed byte all fail.
        let stranger = crate::derive_public_key(&[6_u8; 32]).expect("key");
        assert!(verify_device_manifest(text.as_bytes(), &signature, &stranger).is_err());
        assert!(verify_release_manifest(text.as_bytes(), &signature).is_err());
        let changed = text.replace("0.3.3701", "0.3.3702");
        assert!(verify_device_manifest(changed.as_bytes(), &signature, &key).is_err());

        // A stock manifest is not a device manifest, and the reverse.
        assert!(ReleaseManifest::parse_device(&manifest()).is_err());
        assert!(ReleaseManifest::parse(text.as_bytes()).is_err());
        for refused in [
            format!("{text}device other.tgz 1 {}\n", "b".repeat(64)),
            text.replace("version 0.3.3701", "version 0.3.37-1"),
            text.replace(" 12 ", " 0 "),
            text.replace(source, "main"),
            text.trim_end().to_owned(),
        ] {
            assert!(
                ReleaseManifest::parse_device(refused.as_bytes()).is_err(),
                "{refused:?}"
            );
        }
        assert!(device_manifest("1.0", source, "x.tgz", 1, &"a".repeat(64)).is_err());
    }

    /// Run with `COBALT_RELEASE_REPOSITORY` and `COBALT_PLATFORM_KEY` set to
    /// check an owner's build; without them there is nothing to check.
    #[test]
    fn an_owner_build_reads_only_its_own_releases() {
        if !PLATFORM.owner {
            return;
        }
        assert_ne!(PLATFORM.repository, "BandarLabs/Cobalt");
        assert_ne!(PLATFORM.key_hex, PUBLIC_RELEASE_KEY_HEX);
        assert!(Ed25519PublicKey::from_hex(PLATFORM.key_hex).is_ok());
        assert_eq!(PLATFORM.manifest_file(), DEVICE_MANIFEST_FILE);
        assert!(PLATFORM
            .stable_releases_url()
            .ends_with(&format!("/repos/{}/releases/latest", PLATFORM.repository)));
        // A stock release's metadata is not this build's, whoever signed it.
        assert!(PLATFORM
            .verify_manifest(&manifest(), &"0".repeat(128))
            .is_err());
    }

    #[test]
    fn a_stock_build_reads_the_public_releases_under_the_release_key() {
        if PLATFORM.owner {
            return;
        }
        assert_eq!(PLATFORM.key_hex, PUBLIC_RELEASE_KEY_HEX);
        assert_eq!(
            PLATFORM.stable_releases_url(),
            "https://api.github.com/repos/BandarLabs/Cobalt/releases/latest"
        );
        assert_eq!(PLATFORM.manifest_file(), "cobalt-host-manifest.txt");
        let owner = PlatformSource {
            repository: "someone/Cobalt",
            key_hex: PUBLIC_RELEASE_KEY_HEX,
            owner: true,
        };
        assert_eq!(owner.manifest_file(), DEVICE_MANIFEST_FILE);
        assert_eq!(
            owner.recent_releases_url(20),
            "https://api.github.com/repos/someone/Cobalt/releases?per_page=20"
        );
        // An owner build cannot borrow the release key.
        assert!(owner
            .verify_manifest(&manifest(), &"0".repeat(128))
            .is_err());
    }

    #[test]
    fn release_manifest_binds_both_channels_and_the_device_digest() {
        let parsed = ReleaseManifest::parse(&manifest()).expect("manifest");
        assert!(parsed.allows_channel("stable"));
        assert!(parsed.allows_channel("beta"));
        let device = parsed.device().expect("device");
        assert_eq!(device.name, "cobalt-0.3.4-KoboRoot.tgz");
        assert_eq!(device.sha256, "a".repeat(64));
    }

    #[test]
    fn unsigned_or_malformed_release_metadata_is_refused() {
        assert!(verify_release_manifest(&manifest(), &"0".repeat(128)).is_err());
        let mut malformed = manifest();
        malformed.extend_from_slice(b"version 9.9.9\n");
        assert!(ReleaseManifest::parse(&malformed).is_err());
    }
}
