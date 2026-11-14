use super::super::{
    admit_using, catalog_using, install_using_fault, install_with, installed_using,
    refresh_using_fault, refresh_with, resolve_using, uninstall_using, AppWriteFault,
    CATALOG_SIGNATURE_URL, CATALOG_URL,
};
use super::*;
use kobo_app_store::{
    build_bundle, derive_public_key, sign, CatalogEntry, CatalogEntryInput, Manifest, ManifestInput,
};
use kobo_protocol::UpdateChannel;
use std::collections::BTreeMap;

const RELEASE: [u8; 32] = [9; 32];
const OWNER: [u8; 32] = [42; 32];
const STRANGER: [u8; 32] = [77; 32];
const OWN_URL: &str = "https://apps.example.net/own/cobalt-app-catalog-v2.json";

fn root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "cobalt-private-store-{}-{name}",
        std::process::id()
    ));
    let _ignored = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("root");
    root
}

fn key(seed: &[u8; 32]) -> Ed25519PublicKey {
    derive_public_key(seed).expect("key")
}

fn name_source(root: &Path, url: Option<&str>, seed: &[u8; 32]) {
    fs::create_dir_all(root.join("state")).expect("state");
    let source = Source {
        url: url.map(str::to_owned),
        key: key(seed),
    };
    fs::write(source_file(root), render(&source)).expect("source file");
}

/// What a server holds: every address and the bytes behind it.
#[derive(Default)]
struct Net(BTreeMap<String, Vec<u8>>);

impl Net {
    fn fetch(&self) -> impl FnMut(&str, u32) -> Result<Vec<u8>, DeviceError> + '_ {
        |url, _| self.0.get(url).cloned().ok_or(DeviceError::NotFound)
    }

    /// Publishes a catalog at `url`, signed with `seed`, of packages each
    /// signed with a seed of its own.
    fn publish(&mut self, url: &str, seed: &[u8; 32], apps: &[(&str, &str, &[u8; 32])]) {
        let entries = apps
            .iter()
            .map(|(id, version, signer)| {
                let binary = format!("{id} {version} binary").into_bytes();
                let manifest = Manifest::new_public(ManifestInput {
                    id: (*id).to_owned(),
                    display_name: format!("{id} application"),
                    short_label: (*id).to_owned(),
                    summary: format!("The {id} test application."),
                    version: (*version).to_owned(),
                    minimum_cobalt_version: env!("CARGO_PKG_VERSION").to_owned(),
                    glyph: "note".to_owned(),
                    capabilities: Vec::new(),
                    binary_sha256: kobo_net::sha256::hex_digest(&binary),
                    binary_bytes: binary.len() as u64,
                })
                .expect("manifest");
                let package = build_bundle(&manifest, &binary, signer).expect("bundle");
                let package_url = format!(
                    "https://packages.example.net/{id}-{}.cobalt-app",
                    kobo_net::sha256::hex_digest(&package)
                );
                let entry = CatalogEntry::new(CatalogEntryInput {
                    manifest,
                    package_url: package_url.clone(),
                    package_sha256: kobo_net::sha256::hex_digest(&package),
                    package_bytes: package.len() as u64,
                })
                .expect("entry");
                self.0.insert(package_url, package);
                entry
            })
            .collect();
        let json = Catalog::new(entries).expect("catalog").to_canonical_bytes();
        let signature = format!("{}\n", sign(&json, seed).expect("signature")).into_bytes();
        let signature_url = if url == CATALOG_URL {
            CATALOG_SIGNATURE_URL.to_owned()
        } else {
            format!("{url}.sig")
        };
        self.0.insert(url.to_owned(), json);
        self.0.insert(signature_url, signature);
    }
}

fn find<'a>(entries: &'a [AppInfo], id: &str) -> Option<&'a AppInfo> {
    entries.iter().find(|entry| entry.id == id)
}

fn status(root: &Path) -> String {
    fs::read_to_string(cache_root(root).join(STATUS_FILE)).unwrap_or_default()
}

#[test]
fn a_source_file_round_trips_and_refuses_what_it_should() {
    let source = Source {
        url: Some(OWN_URL.to_owned()),
        key: key(&OWNER),
    };
    assert_eq!(parse(&render(&source)), Ok(source.clone()));
    let kept = Source {
        url: None,
        ..source
    };
    assert_eq!(parse(&render(&kept)), Ok(kept));

    let owner = key(&OWNER).to_hex();
    for refused in [
        String::new(),
        format!("url={OWN_URL}\n"),
        format!("key={}\n", kobo_app_store::PUBLIC_RELEASE_KEY_HEX),
        format!("key={}\n", owner.to_uppercase()),
        format!("key={owner}\nkey={owner}\n"),
        format!("key={owner}\ntrust=everything\n"),
        format!("url=http://apps.example.net/catalog.json\nkey={owner}\n"),
        format!("url=https://user:pass@apps.example.net/catalog.json\nkey={owner}\n"),
        format!("url=https://apps.example.net/catalog.json?token=1\nkey={owner}\n"),
        format!("url={OWN_URL}\nurl={OWN_URL}\nkey={owner}\n"),
    ] {
        assert!(parse(&refused).is_err(), "{refused:?} should be refused");
    }
}

#[test]
fn both_catalogs_are_listed_and_the_owners_is_marked() {
    let root = root("listed");
    name_source(&root, Some(OWN_URL), &OWNER);
    let mut net = Net::default();
    net.publish(CATALOG_URL, &RELEASE, &[("word-count", "1.0.0", &RELEASE)]);
    net.publish(OWN_URL, &OWNER, &[("own-notes", "0.1.0", &OWNER)]);

    let listed = refresh_with(&root, &key(&RELEASE), net.fetch()).expect("refresh");
    let public = find(&listed, "word-count").expect("public entry");
    let own = find(&listed, "own-notes").expect("own entry");
    assert_eq!(public.summary, "The word-count test application.");
    assert_eq!(own.summary, "Private. The own-notes test application.");
    assert!(own.installed_version.is_none());
    assert_eq!(status(&root), "ok: 1 application(s)\n");

    // The cached listing says the same without the network.
    let cached = catalog_using(&root, UpdateChannel::Stable, &key(&RELEASE)).expect("cached");
    assert_eq!(cached, listed);
}

#[test]
fn an_owner_package_installs_runs_updates_and_uninstalls() {
    let root = root("installs");
    name_source(&root, Some(OWN_URL), &OWNER);
    let release = key(&RELEASE);
    let mut net = Net::default();
    net.publish(CATALOG_URL, &RELEASE, &[("word-count", "1.0.0", &RELEASE)]);
    net.publish(OWN_URL, &OWNER, &[("own-notes", "0.1.0", &OWNER)]);
    refresh_with(&root, &release, net.fetch()).expect("refresh");

    install_with(&root, "own-notes", &release, net.fetch()).expect("install own package");
    install_with(&root, "word-count", &release, net.fetch()).expect("install public package");
    let installed = installed_using(&root, &release).expect("installed");
    assert_eq!(
        find(&installed, "own-notes").and_then(|app| app.installed_version.as_deref()),
        Some("0.1.0")
    );
    assert!(find(&installed, "word-count").is_some());
    resolve_using(&root, "own-notes", &release).expect("resolve");
    let admitted = admit_using(&root, "own-notes", &release).expect("admit");
    assert_eq!(admitted.binary, b"own-notes 0.1.0 binary");

    net.publish(OWN_URL, &OWNER, &[("own-notes", "0.2.0", &OWNER)]);
    let listed = refresh_with(&root, &release, net.fetch()).expect("refresh again");
    assert!(find(&listed, "own-notes").expect("listed").has_update());
    install_with(&root, "own-notes", &release, net.fetch()).expect("update");
    let admitted = admit_using(&root, "own-notes", &release).expect("admit update");
    assert_eq!(admitted.binary, b"own-notes 0.2.0 binary");

    uninstall_using(&root, "own-notes", &release).expect("uninstall");
    let installed = installed_using(&root, &release).expect("installed after");
    assert!(find(&installed, "own-notes").is_none());
    assert!(find(&installed, "word-count").is_some());
}

#[test]
fn without_a_source_file_only_the_release_key_is_believed() {
    let root = root("stock");
    let release = key(&RELEASE);
    let mut net = Net::default();
    net.publish(CATALOG_URL, &RELEASE, &[("word-count", "1.0.0", &RELEASE)]);
    net.publish(OWN_URL, &OWNER, &[("own-notes", "0.1.0", &OWNER)]);

    let listed = refresh_with(&root, &release, net.fetch()).expect("refresh");
    assert!(find(&listed, "own-notes").is_none());
    assert_eq!(
        install_with(&root, "own-notes", &release, net.fetch()),
        Err(DeviceError::NotFound)
    );
    assert_eq!(status(&root), "");
    assert!(!cache_root(&root).join(CACHE_NAME).exists());
}

#[test]
fn a_catalog_signed_by_anyone_else_is_not_the_owners() {
    let root = root("stranger");
    name_source(&root, Some(OWN_URL), &OWNER);
    let release = key(&RELEASE);
    let mut net = Net::default();
    net.publish(CATALOG_URL, &RELEASE, &[("word-count", "1.0.0", &RELEASE)]);
    for signer in [&STRANGER, &RELEASE] {
        net.publish(OWN_URL, signer, &[("own-notes", "0.1.0", signer)]);
        let listed = refresh_with(&root, &release, net.fetch()).expect("public refresh");
        assert!(find(&listed, "word-count").is_some());
        assert!(find(&listed, "own-notes").is_none());
        assert_eq!(status(&root), "failed: Integrity\n");
        assert_eq!(
            install_with(&root, "own-notes", &release, net.fetch()),
            Err(DeviceError::NotFound)
        );
    }
}

#[test]
fn an_owner_entry_answers_only_to_the_owners_key() {
    let root = root("package-key");
    name_source(&root, Some(OWN_URL), &OWNER);
    let release = key(&RELEASE);
    let mut net = Net::default();
    net.publish(CATALOG_URL, &RELEASE, &[("word-count", "1.0.0", &RELEASE)]);
    // The owner's catalog is genuine, but the packages it lists were signed by
    // a stranger and by the release key.
    for signer in [&STRANGER, &RELEASE] {
        net.publish(OWN_URL, &OWNER, &[("own-notes", "0.1.0", signer)]);
        refresh_with(&root, &release, net.fetch()).expect("refresh");
        assert_eq!(
            install_with(&root, "own-notes", &release, net.fetch()),
            Err(DeviceError::Integrity)
        );
        assert!(installed_using(&root, &release)
            .expect("installed")
            .is_empty());
    }
}

#[test]
fn the_owners_key_cannot_speak_for_the_public_catalog() {
    let root = root("public-key");
    name_source(&root, Some(OWN_URL), &OWNER);
    let release = key(&RELEASE);
    let mut net = Net::default();
    // The public address serves a catalog signed with the owner's key.
    net.publish(CATALOG_URL, &OWNER, &[("word-count", "9.9.9", &OWNER)]);
    assert_eq!(
        refresh_with(&root, &release, net.fetch()),
        Err(DeviceError::Integrity)
    );

    // A public entry's package signed with the owner's key is refused too.
    net.publish(CATALOG_URL, &RELEASE, &[("word-count", "1.0.0", &OWNER)]);
    refresh_with(&root, &release, net.fetch()).expect("refresh");
    assert_eq!(
        install_with(&root, "word-count", &release, net.fetch()),
        Err(DeviceError::Integrity)
    );
}

#[test]
fn an_identity_the_public_catalog_lists_is_the_public_catalogs() {
    let root = root("collision");
    name_source(&root, Some(OWN_URL), &OWNER);
    let release = key(&RELEASE);
    let mut net = Net::default();
    net.publish(CATALOG_URL, &RELEASE, &[("word-count", "1.0.0", &RELEASE)]);
    net.publish(OWN_URL, &OWNER, &[("word-count", "5.0.0", &OWNER)]);

    let listed = refresh_with(&root, &release, net.fetch()).expect("refresh");
    let entries: Vec<_> = listed.iter().filter(|e| e.id == "word-count").collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].version, "1.0.0");
    assert_eq!(entries[0].summary, "The word-count test application.");

    install_with(&root, "word-count", &release, net.fetch()).expect("install");
    let admitted = admit_using(&root, "word-count", &release).expect("admit");
    assert_eq!(admitted.binary, b"word-count 1.0.0 binary");
}

#[test]
fn the_owners_server_being_down_costs_nothing_public() {
    let root = root("down");
    name_source(&root, Some(OWN_URL), &OWNER);
    let release = key(&RELEASE);
    let mut net = Net::default();
    net.publish(CATALOG_URL, &RELEASE, &[("word-count", "1.0.0", &RELEASE)]);
    net.publish(OWN_URL, &OWNER, &[("own-notes", "0.1.0", &OWNER)]);
    refresh_with(&root, &release, net.fetch()).expect("first refresh");

    let listed = refresh_with(&root, &release, |url, _| {
        if url.starts_with(OWN_URL) {
            Err(DeviceError::Unreachable)
        } else {
            net.0.get(url).cloned().ok_or(DeviceError::NotFound)
        }
    })
    .expect("public refresh");
    assert!(find(&listed, "word-count").is_some());
    // The last verified copy of the owner's catalog is still offered.
    assert!(find(&listed, "own-notes").is_some());
    assert_eq!(status(&root), "failed: Unreachable\n");
}

#[test]
fn the_owners_catalog_serves_a_reader_that_never_reached_the_public_one() {
    let root = root("alone");
    name_source(&root, Some(OWN_URL), &OWNER);
    let release = key(&RELEASE);
    let mut net = Net::default();
    net.publish(OWN_URL, &OWNER, &[("own-notes", "0.1.0", &OWNER)]);

    // The public catalog is missing, which is the refresh's error to report.
    assert_eq!(
        refresh_with(&root, &release, net.fetch()),
        Err(DeviceError::NotFound)
    );
    install_with(&root, "own-notes", &release, net.fetch()).expect("install");
    assert_eq!(
        install_with(&root, "word-count", &release, net.fetch()),
        Err(DeviceError::NotFound)
    );
}

#[test]
fn a_source_without_an_address_keeps_what_it_installed_and_offers_nothing() {
    let root = root("kept");
    name_source(&root, Some(OWN_URL), &OWNER);
    let release = key(&RELEASE);
    let mut net = Net::default();
    net.publish(CATALOG_URL, &RELEASE, &[("word-count", "1.0.0", &RELEASE)]);
    net.publish(
        OWN_URL,
        &OWNER,
        &[
            ("own-notes", "0.1.0", &OWNER),
            ("own-tool", "1.0.0", &OWNER),
        ],
    );
    refresh_with(&root, &release, net.fetch()).expect("refresh");
    install_with(&root, "own-notes", &release, net.fetch()).expect("install");

    name_source(&root, None, &OWNER);
    let listed = refresh_with(&root, &release, net.fetch()).expect("refresh without address");
    // Installed, so still listed, from its own manifest and unmarked.
    let kept = find(&listed, "own-notes").expect("still installed");
    assert_eq!(kept.installed_version.as_deref(), Some("0.1.0"));
    assert!(find(&listed, "own-tool").is_none());
    admit_using(&root, "own-notes", &release).expect("still runs");
    assert_eq!(
        install_with(&root, "own-tool", &release, net.fetch()),
        Err(DeviceError::NotFound)
    );
}

#[test]
fn forgetting_the_key_stops_believing_what_it_signed() {
    let root = root("forgotten");
    name_source(&root, Some(OWN_URL), &OWNER);
    let release = key(&RELEASE);
    let mut net = Net::default();
    net.publish(OWN_URL, &OWNER, &[("own-notes", "0.1.0", &OWNER)]);
    let _public_missing = refresh_with(&root, &release, net.fetch());
    install_with(&root, "own-notes", &release, net.fetch()).expect("install");
    admit_using(&root, "own-notes", &release).expect("runs");

    fs::remove_file(source_file(&root)).expect("forget");
    assert!(admit_using(&root, "own-notes", &release).is_err());
    assert_eq!(
        installed_using(&root, &release),
        Err(DeviceError::Integrity)
    );
    // Another key is no better than none.
    name_source(&root, Some(OWN_URL), &STRANGER);
    assert!(admit_using(&root, "own-notes", &release).is_err());
}

#[test]
fn an_injected_write_fault_leaves_the_owners_cache_alone() {
    let root = root("fault");
    name_source(&root, Some(OWN_URL), &OWNER);
    let release = key(&RELEASE);
    let mut net = Net::default();
    net.publish(CATALOG_URL, &RELEASE, &[("word-count", "1.0.0", &RELEASE)]);
    net.publish(OWN_URL, &OWNER, &[("own-notes", "0.1.0", &OWNER)]);

    let faulted = refresh_using_fault(
        &root,
        UpdateChannel::Stable,
        &release,
        net.fetch(),
        Some(AppWriteFault::NoRoom),
    );
    assert_eq!(faulted, Err(DeviceError::Backend));
    assert!(!cache_root(&root).join(CACHE_NAME).exists());

    refresh_with(&root, &release, net.fetch()).expect("refresh");
    let faulted = install_using_fault(
        &root,
        "own-notes",
        UpdateChannel::Stable,
        &release,
        net.fetch(),
        Some(AppWriteFault::NoRoom),
    );
    assert_eq!(faulted, Err(DeviceError::Backend));
    assert!(installed_using(&root, &release)
        .expect("installed")
        .is_empty());
}

#[test]
fn a_damaged_or_oversized_source_file_names_no_source() {
    let root = root("damaged");
    fs::create_dir_all(root.join("state")).expect("state");
    fs::write(source_file(&root), "key=not-a-key\n").expect("write");
    assert_eq!(source(&root), None);
    let padded = format!(
        "{}{}",
        render(&Source {
            url: None,
            key: key(&OWNER)
        }),
        "\n".repeat(5000)
    );
    fs::write(source_file(&root), padded).expect("write");
    assert_eq!(source(&root), None);
    fs::remove_file(source_file(&root)).expect("remove");
    fs::create_dir(source_file(&root)).expect("directory in its place");
    assert_eq!(source(&root), None);
}

fn save_password(root: &Path, password: &str) {
    let directory = root.join("secrets").join("apps").join(PASSWORD_APP);
    fs::create_dir_all(&directory).expect("secrets");
    fs::write(directory.join(PASSWORD_NAME), password).expect("password");
}

#[test]
fn the_password_goes_to_the_owners_server_and_nowhere_else() {
    let root = root("password");
    let on_server = "https://apps.example.net/own/drafter.cobalt-app";
    // No source and no password: nothing is sent anywhere.
    assert_eq!(authorization(&root, OWN_URL), None);
    save_password(&root, "open sesame\n");
    assert_eq!(authorization(&root, OWN_URL), None);

    name_source(&root, Some(OWN_URL), &OWNER);
    let expected = Some("Bearer open sesame".to_owned());
    assert_eq!(authorization(&root, OWN_URL), expected);
    assert_eq!(authorization(&root, &format!("{OWN_URL}.sig")), expected);
    assert_eq!(authorization(&root, on_server), expected);
    for elsewhere in [
        CATALOG_URL,
        "https://apps.example.net.attacker.invalid/own/drafter.cobalt-app",
        "https://apps.example.net:8443/own/drafter.cobalt-app",
        "https://packages.example.net/drafter.cobalt-app",
        "http://apps.example.net/own/drafter.cobalt-app",
    ] {
        assert_eq!(authorization(&root, elsewhere), None, "{elsewhere}");
    }

    // A source that asks for nothing sends nothing.
    name_source(&root, None, &OWNER);
    assert_eq!(authorization(&root, on_server), None);
}

#[test]
fn an_unusable_password_is_no_password() {
    let root = root("bad-password");
    name_source(&root, Some(OWN_URL), &OWNER);
    for unusable in ["", "  \n", "two\nlines", &"x".repeat(600)] {
        save_password(&root, unusable);
        assert_eq!(authorization(&root, OWN_URL), None, "{unusable:?}");
    }
}
