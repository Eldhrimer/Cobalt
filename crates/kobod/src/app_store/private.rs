//! The owner's own signed catalog, read beside the public one.
//!
//! A reader may name one more place applications come from: an HTTPS address
//! and the Ed25519 public key its catalog and packages are signed with. It is
//! for applications their author does not publish: the same signed bundles
//! and the same atomic installs, from a server the owner runs.
//!
//! Nothing here loosens what the public Store accepts. The release key still
//! vouches for the public catalog and every package in it, and a catalog or a
//! package signed with the owner's key is only ever accepted as the owner's:
//!
//! - The owner's catalog is fetched, verified and cached on its own, under its
//!   own key, and never stands in for the public one.
//! - An identity the public catalog lists is the public catalog's. The owner's
//!   entry of the same name is not offered and cannot be installed.
//! - An installed application is believed when its manifest is signed by the
//!   release key or by the owner's key, and by nothing else.
//!
//! The owner's server may ask for a password. The owner types it once, in
//! Settings, and the runtime sends it as a bearer token to the server the
//! catalog's address names and to no other: not to the public Store, and not
//! to a package address on another server even when the owner's catalog
//! lists one. The server decides what the password guards; the signatures
//! decide what is installed either way.
//!
//! With no source file on the reader, none of this runs and the Store behaves
//! exactly as it does without this module.
//!
//! The file is `state/private-store` under the Cobalt folder, two lines:
//!
//! ```text
//! url=https://apps.example.net/cobalt-app-catalog-v2.json
//! key=<64 lowercase hexadecimal characters>
//! ```
//!
//! The signature is read from the same address with `.sig` after it. `url`
//! may be left out: the reader then asks for nothing and offers nothing, and
//! goes on running what was installed from the source while it had one.

use super::{
    cache_root, manifest_info, read_catalog_directory, recover_named_catalog_cache, verify,
    verify_catalog, write_named_catalog_cache, Catalog, DetachedSignature, Ed25519PublicKey,
    CATALOG_LIMIT, SIGNATURE_LIMIT,
};
use kobo_protocol::{AppInfo, DeviceError};
use std::fs;
use std::path::{Path, PathBuf};

/// The source file's name, inside the Cobalt folder's `state` directory.
pub const SOURCE_FILE: &str = "private-store";
/// Where the last refresh of the owner's catalog says how it went, inside the
/// Cobalt folder's `store` directory.
pub const STATUS_FILE: &str = "private-status";

/// The name the catalog's password is saved under, in Settings' own secret
/// namespace, which is where a secret typed on its screen is written.
pub const PASSWORD_NAME: &str = "private-store";
/// The application whose secret namespace holds the password.
pub const PASSWORD_APP: &str = "settings";

const CACHE_NAME: &str = "catalog-private";
const PASSWORD_LIMIT: u64 = 512;
const SOURCE_LIMIT: u64 = 4096;
/// What the Store shows in front of the summary of an application offered by
/// the owner's source, so it is never mistaken for a public one.
const TAG: &str = "Private. ";
/// The longest summary the protocol carries.
const SUMMARY_LIMIT: usize = 512;

/// One extra place applications come from, and the key that vouches for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source {
    /// The catalog's address. Without one nothing is fetched or offered.
    pub url: Option<String>,
    /// The key the catalog, and every package it lists, is signed with.
    pub key: Ed25519PublicKey,
}

/// Reads a source file's text.
///
/// # Errors
///
/// Says what is wrong with the first line that is not a `url` or a `key`, is
/// given twice, or does not hold what its name promises. A file without a
/// `key` names no source.
pub fn parse(text: &str) -> Result<Source, &'static str> {
    let mut url = None;
    let mut key = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match line.split_once('=') {
            Some(("url", value)) if url.is_none() => url = Some(checked_url(value)?),
            Some(("key", value)) if key.is_none() => key = Some(checked_key(value)?),
            Some(("url" | "key", _)) => return Err("a line is given twice"),
            _ => return Err("every line is url=ADDRESS or key=PUBLIC_KEY"),
        }
    }
    Ok(Source {
        url,
        key: key.ok_or("the source has no key")?,
    })
}

/// The text [`parse`] reads back as `source`.
#[must_use]
pub fn render(source: &Source) -> String {
    let mut text = String::new();
    if let Some(url) = &source.url {
        text.push_str("url=");
        text.push_str(url);
        text.push('\n');
    }
    text.push_str("key=");
    text.push_str(&source.key.to_hex());
    text.push('\n');
    text
}

fn checked_url(value: &str) -> Result<String, &'static str> {
    // The signature's address is this one with `.sig` after it, which a query
    // or a fragment would put in the wrong place.
    if value.len() > kobo_app_store::MAX_PACKAGE_URL_BYTES
        || value.contains(['?', '#'])
        || kobo_net::parse(value).is_err()
    {
        return Err("the address must be a plain HTTPS address without credentials");
    }
    Ok(value.to_owned())
}

fn checked_key(value: &str) -> Result<Ed25519PublicKey, &'static str> {
    if value == kobo_app_store::PUBLIC_RELEASE_KEY_HEX {
        // The release key already vouches for the public catalog. A second
        // source under it would let any address pass for the public Store.
        return Err("the source needs a key of its own, not the release key");
    }
    Ed25519PublicKey::from_hex(value)
        .map_err(|_| "the key must be 64 lowercase hexadecimal characters")
}

/// Where the source file lives for the Cobalt folder `root`.
#[must_use]
pub fn source_file(root: &Path) -> PathBuf {
    root.join("state").join(SOURCE_FILE)
}

/// The source this reader names, if it names one that reads cleanly.
///
/// A file that is missing, is not an ordinary file, is too long or does not
/// parse names no source: the reader then trusts the release key alone.
#[must_use]
pub fn source(root: &Path) -> Option<Source> {
    let path = source_file(root);
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > SOURCE_LIMIT {
        return None;
    }
    parse(&fs::read_to_string(path).ok()?).ok()
}

/// The password the owner saved for their server, if they saved one.
fn password(root: &Path) -> Option<String> {
    let path = kobo_policy::credentials::app_secret_path(
        &root.join("secrets"),
        PASSWORD_APP,
        PASSWORD_NAME,
    )?;
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > PASSWORD_LIMIT {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let password = text.trim();
    (!password.is_empty() && !password.chars().any(char::is_control)).then(|| password.to_owned())
}

/// The `Authorization` value to send with a request for `url`, which is one
/// only when `url` is on the server the owner's catalog is on.
#[must_use]
pub fn authorization(root: &Path, url: &str) -> Option<String> {
    let source = source(root)?;
    let home = kobo_net::parse(source.url.as_deref()?).ok()?;
    if !kobo_net::has_origin(url, &home.host, home.port) {
        return None;
    }
    Some(format!("Bearer {}", password(root)?))
}

/// Fetches for the Store on a reader: the owner's password goes to the
/// owner's server, and every other address is asked without one.
///
/// # Errors
///
/// The transport's own bounded errors.
pub fn fetch(root: &Path, url: &str, maximum: u32) -> Result<Vec<u8>, kobo_protocol::TaskError> {
    match authorization(root, url) {
        Some(value) => kobo_net::fetch_from(url, 0, maximum, Some(("Authorization", &value)), &[]),
        None => kobo_net::fetch(url, maximum),
    }
}

/// Whether the owner's key signed these manifest bytes.
///
/// `directory` is an installed application's folder, `apps/<id>` under the
/// Cobalt folder or that folder mid-transaction, which is how the source file
/// is found without every caller handing the Cobalt folder down.
pub(super) fn vouches(directory: &Path, manifest: &[u8], signature: &DetachedSignature) -> bool {
    directory
        .parent()
        .and_then(Path::parent)
        .and_then(source)
        .is_some_and(|source| verify(manifest, signature, &source.key).is_ok())
}

/// Fetches, verifies and caches the owner's catalog, when there is one to ask.
///
/// Its failure is recorded in [`STATUS_FILE`] and goes no further. The public
/// catalog's refresh is the one the Store reports, and the owner's server
/// being down must not cost the reader the public Store.
pub(super) fn refresh(
    root: &Path,
    fetch: &mut impl FnMut(&str, u32) -> Result<Vec<u8>, DeviceError>,
    faulted: bool,
) {
    let Some(source) = source(root) else {
        return;
    };
    let Some(url) = source.url.as_deref() else {
        return;
    };
    let outcome = pull(root, url, &source.key, fetch, faulted);
    let status = match outcome {
        Ok(count) => format!("ok: {count} application(s)\n"),
        Err(error) => format!("failed: {error:?}\n"),
    };
    // Best effort: the status is for the owner to read, and nothing reads it
    // back to decide anything.
    let store = cache_root(root);
    let _ignored = fs::create_dir_all(&store);
    let _ignored = fs::write(store.join(STATUS_FILE), status);
}

fn pull(
    root: &Path,
    url: &str,
    key: &Ed25519PublicKey,
    fetch: &mut impl FnMut(&str, u32) -> Result<Vec<u8>, DeviceError>,
    faulted: bool,
) -> Result<usize, DeviceError> {
    let json = fetch(url, CATALOG_LIMIT)?;
    let signature = fetch(&format!("{url}.sig"), SIGNATURE_LIMIT)?;
    let catalog = verify_catalog(&json, &signature, key)?;
    if faulted {
        return Err(DeviceError::Backend);
    }
    write_named_catalog_cache(root, CACHE_NAME, &json, &signature)?;
    Ok(catalog.entries().len())
}

/// The owner's catalog as last verified, with the key that verified it.
fn cached(root: &Path) -> Option<(Catalog, Ed25519PublicKey)> {
    let source = source(root)?;
    source.url.as_ref()?;
    recover_named_catalog_cache(root, CACHE_NAME, &source.key).ok()?;
    let catalog = read_catalog_directory(&cache_root(root).join(CACHE_NAME), &source.key).ok()?;
    Some((catalog, source.key))
}

/// Adds what the owner's catalog offers to a listing of the public one.
///
/// `entries` holds the public catalog's applications, and anything already
/// there keeps its place: an identity the public catalog lists is never
/// replaced by the owner's entry of the same name.
pub(super) fn offer(root: &Path, installed: &[AppInfo], entries: &mut Vec<AppInfo>) {
    let Some((catalog, _)) = cached(root) else {
        return;
    };
    for entry in catalog.entries() {
        let manifest = entry.manifest();
        if entries.iter().any(|listed| listed.id == manifest.id()) {
            continue;
        }
        let version = installed
            .iter()
            .find(|app| app.id == manifest.id())
            .and_then(|app| app.installed_version.as_deref());
        let mut info = manifest_info(manifest, version);
        if info.summary.len() + TAG.len() <= SUMMARY_LIMIT {
            info.summary.insert_str(0, TAG);
        }
        entries.push(info);
    }
}

/// The catalog an install of `id` reads its entry from, and the key that
/// entry's package must be signed with.
///
/// The public catalog answers for every identity it lists. Only an identity
/// it does not list is looked for in the owner's catalog, and a package found
/// there is held to the owner's key, never to the release key.
///
/// # Errors
///
/// The public catalog's own error, when it could not be read and the owner's
/// catalog does not list `id` either.
pub(super) fn catalog_for(
    root: &Path,
    id: &str,
    public: Result<Catalog, DeviceError>,
    key: &Ed25519PublicKey,
) -> Result<(Catalog, Ed25519PublicKey), DeviceError> {
    let lists = |catalog: &Catalog| {
        catalog
            .entries()
            .iter()
            .any(|entry| entry.manifest().id() == id)
    };
    if !public.as_ref().is_ok_and(lists) {
        if let Some((catalog, own)) = cached(root).filter(|(catalog, _)| lists(catalog)) {
            return Ok((catalog, own));
        }
    }
    public.map(|catalog| (catalog, *key))
}

#[cfg(test)]
mod tests;
