//! An owner's private catalog: make its key, name it on a reader, publish it.
//!
//! The runtime half is `kobod::app_store::private`. A reader that holds a
//! source file reads one more signed catalog beside the public Store's, from
//! an address the owner runs, and believes it only under the owner's key.

use kobod::app_store::private::{parse, render, Source, SOURCE_FILE, STATUS_FILE};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const DEVICE_ROOT: &str = "/mnt/onboard/.adds/cobalt";
const USAGE: &str = "usage: kobo private-store keygen --seed PATH\n\
     \x20      kobo private-store set --url HTTPS_URL (--key HEX | --seed PATH) TARGET\n\
     \x20      kobo private-store show TARGET\n\
     \x20      kobo private-store remove [--forget-key] TARGET\n\
     \x20      kobo private-store publish --app ID [--app ID ...] --registry PATH --seed PATH\n\
     \x20             --out DIR --base-url HTTPS_URL [--prebuilt-dir PATH | --artifact-dir PATH]\n\
     \n\
     TARGET is --device IP, --volume PATH (a reader mounted over USB) or\n\
     --root PATH (a Cobalt folder, such as a simulator Store fixture).\n\
     \n\
     keygen writes a new signing seed and prints its public key. Keep the seed\n\
     off the reader and off the server: whoever holds it can sign applications\n\
     your reader will run.\n\
     set names your catalog on a reader: the address of its\n\
     cobalt-app-catalog-v2.json and the public key it is signed with.\n\
     remove stops the reader asking your server and offering its applications;\n\
     the ones already installed keep running. --forget-key also stops trusting\n\
     the key: uninstall those applications first, or the reader will refuse to\n\
     list what is installed until the key is back.\n\
     publish builds, signs and lays out the catalog and packages for the named\n\
     applications in DIR, ready to serve as static files at the base address.";

enum Target {
    Device(String),
    Folder(PathBuf),
}

pub fn command(arguments: &[String]) -> Result<(), String> {
    if super::wants_help(arguments) {
        return super::print_command_help(USAGE);
    }
    let (verb, rest) = arguments.split_first().ok_or_else(|| USAGE.to_owned())?;
    match verb.as_str() {
        "keygen" => keygen(rest),
        "set" => set(rest),
        "show" => show(rest),
        "remove" => remove(rest),
        "publish" => publish(rest),
        _ => Err(USAGE.to_owned()),
    }
}

/// Splits `arguments` into the reader they name and everything else.
fn take_target(arguments: &[String]) -> Result<(Target, Vec<String>), String> {
    let mut target = None;
    let mut others = Vec::new();
    let mut rest = arguments;
    while let Some((flag, tail)) = rest.split_first() {
        let named = match flag.as_str() {
            flag if super::is_device_flag(flag) => {
                let host = tail.first().ok_or_else(|| USAGE.to_owned())?;
                if !super::valid_device_host(host) {
                    return Err("device host contains unsupported characters".to_owned());
                }
                Some(Target::Device(host.clone()))
            }
            "--volume" => Some(Target::Folder(
                Path::new(tail.first().ok_or_else(|| USAGE.to_owned())?)
                    .join(".adds")
                    .join("cobalt"),
            )),
            "--root" => Some(Target::Folder(PathBuf::from(
                tail.first().ok_or_else(|| USAGE.to_owned())?,
            ))),
            _ => None,
        };
        if let Some(named) = named {
            if target.replace(named).is_some() {
                return Err(USAGE.to_owned());
            }
            rest = &tail[1..];
        } else {
            others.push(flag.clone());
            rest = tail;
        }
    }
    Ok((target.ok_or_else(|| USAGE.to_owned())?, others))
}

fn remote(host: &str, script: &str, doing: &str) -> Result<String, String> {
    let output =
        super::run_remote_shell(&format!("root@{host}"), script, super::DEVICE_PROBE_TIMEOUT)?;
    if !output.status.success() {
        return Err(super::remote_shell_error(
            doing.to_owned(),
            &output.stdout,
            &output.stderr,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn keygen(arguments: &[String]) -> Result<(), String> {
    let [flag, path] = arguments else {
        return Err(USAGE.to_owned());
    };
    if flag != "--seed" {
        return Err(USAGE.to_owned());
    }
    let mut seed = [0_u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut seed))
        .map_err(|error| format!("read /dev/urandom: {error}"))?;
    let public = kobo_app_store::derive_public_key(&seed).map_err(|error| error.to_string())?;
    let hex = seed.iter().fold(String::new(), |mut hex, byte| {
        hex.push_str(&format!("{byte:02x}"));
        hex
    });
    // Never over an existing file: a seed that is replaced takes every
    // application it signed with it.
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("create {path}: {error}"))?;
    file.write_all(format!("{hex}\n").as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("write {path}: {error}"))?;
    println!("Wrote the signing seed to {path}. Keep it private and backed up.");
    println!("Public key: {public}");
    Ok(())
}

fn set(arguments: &[String]) -> Result<(), String> {
    let (target, others) = take_target(arguments)?;
    let mut url = None;
    let mut key = None;
    let mut rest = others.as_slice();
    while let Some((flag, tail)) = rest.split_first() {
        let (value, tail) = tail.split_first().ok_or_else(|| USAGE.to_owned())?;
        match flag.as_str() {
            "--url" if url.is_none() => url = Some(value.clone()),
            "--key" if key.is_none() => key = Some(value.clone()),
            "--seed" if key.is_none() => {
                let seed = super::read_signing_seed(Path::new(value))?;
                let public =
                    kobo_app_store::derive_public_key(&seed).map_err(|error| error.to_string())?;
                key = Some(public.to_string());
            }
            _ => return Err(USAGE.to_owned()),
        }
        rest = tail;
    }
    let (Some(url), Some(key)) = (url, key) else {
        return Err(USAGE.to_owned());
    };
    // Parsed here by the runtime's own reader, so nothing reaches the reader
    // that it would then ignore.
    let source = parse(&format!("url={url}\nkey={key}\n"))
        .map_err(|reason| format!("that source cannot be used: {reason}"))?;
    let text = render(&source);
    match target {
        Target::Device(host) => {
            let script = format!(
                "set -e\n\
                 mkdir -p {DEVICE_ROOT}/state\n\
                 cat > {DEVICE_ROOT}/state/{SOURCE_FILE}.next <<'COBALT_PRIVATE_STORE_ENDS_HERE'\n\
                 {text}COBALT_PRIVATE_STORE_ENDS_HERE\n\
                 mv {DEVICE_ROOT}/state/{SOURCE_FILE}.next {DEVICE_ROOT}/state/{SOURCE_FILE}\n\
                 sync\n"
            );
            remote(&host, &script, "name the private source")?;
            println!("Named the private source on {host}.");
        }
        Target::Folder(root) => {
            write_source(&root, &text)?;
            println!("Named the private source in {}.", root.display());
        }
    }
    println!("Open Store on the reader and refresh: its applications are listed as Private.");
    Ok(())
}

fn write_source(root: &Path, text: &str) -> Result<(), String> {
    let state = root.join("state");
    fs::create_dir_all(&state).map_err(|error| format!("create {}: {error}", state.display()))?;
    let path = state.join(SOURCE_FILE);
    let next = state.join(format!("{SOURCE_FILE}.next"));
    fs::write(&next, text).map_err(|error| format!("write {}: {error}", next.display()))?;
    fs::rename(&next, &path).map_err(|error| format!("write {}: {error}", path.display()))
}

fn show(arguments: &[String]) -> Result<(), String> {
    let (target, others) = take_target(arguments)?;
    if !others.is_empty() {
        return Err(USAGE.to_owned());
    }
    let (source, status) = match target {
        Target::Device(host) => {
            let script = format!(
                "cat {DEVICE_ROOT}/state/{SOURCE_FILE} 2>/dev/null || true\n\
                 echo COBALT_PRIVATE_STORE_STATUS\n\
                 cat {DEVICE_ROOT}/store/{STATUS_FILE} 2>/dev/null || true\n"
            );
            let output = remote(&host, &script, "read the private source")?;
            let (source, status) = output
                .split_once("COBALT_PRIVATE_STORE_STATUS\n")
                .unwrap_or((output.as_str(), ""));
            (source.to_owned(), status.to_owned())
        }
        Target::Folder(root) => (
            fs::read_to_string(root.join("state").join(SOURCE_FILE)).unwrap_or_default(),
            fs::read_to_string(root.join("store").join(STATUS_FILE)).unwrap_or_default(),
        ),
    };
    if source.trim().is_empty() {
        println!("No private source is named. The reader trusts the public Store alone.");
        return Ok(());
    }
    match parse(&source) {
        Ok(Source { url, key }) => {
            match url {
                Some(url) => println!("Catalog: {url}"),
                None => println!("Catalog: none (installed applications are kept)"),
            }
            println!("Public key: {key}");
        }
        Err(reason) => println!("The source file is not usable and is ignored: {reason}"),
    }
    match status.trim() {
        "" => println!("Last refresh: none yet"),
        status => println!("Last refresh: {status}"),
    }
    Ok(())
}

fn remove(arguments: &[String]) -> Result<(), String> {
    let (target, others) = take_target(arguments)?;
    let forget = match others.as_slice() {
        [] => false,
        [flag] if flag == "--forget-key" => true,
        _ => return Err(USAGE.to_owned()),
    };
    match target {
        Target::Device(host) => {
            let file = format!("{DEVICE_ROOT}/state/{SOURCE_FILE}");
            let script = if forget {
                format!("rm -f {file}\nsync\n")
            } else {
                format!(
                    "set -e\n\
                     [ -f {file} ] || exit 0\n\
                     grep '^key=' {file} > {file}.next\n\
                     mv {file}.next {file}\n\
                     sync\n"
                )
            };
            remote(&host, &script, "remove the private source")?;
        }
        Target::Folder(root) => {
            let path = root.join("state").join(SOURCE_FILE);
            if forget {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(format!("remove {}: {error}", path.display())),
                }
            } else if let Ok(text) = fs::read_to_string(&path) {
                let source = parse(&text)
                    .map_err(|reason| format!("{} is not usable: {reason}", path.display()))?;
                write_source(
                    &root,
                    &render(&Source {
                        url: None,
                        ..source
                    }),
                )?;
            }
        }
    }
    if forget {
        println!(
            "The private source and its key are gone. Applications installed from it no \
             longer verify."
        );
    } else {
        println!(
            "The reader no longer asks your server. Applications already installed from it \
             keep running."
        );
    }
    Ok(())
}

fn publish(arguments: &[String]) -> Result<(), String> {
    let mut chosen = Vec::new();
    let mut passed = Vec::new();
    let mut rest = arguments;
    while let Some((flag, tail)) = rest.split_first() {
        if flag == "--app" {
            let (id, tail) = tail.split_first().ok_or_else(|| USAGE.to_owned())?;
            if !kobo_protocol::valid_app_id(id) {
                return Err(format!("{id:?} is not an application identity"));
            }
            chosen.push(id.clone());
            rest = tail;
        } else {
            passed.push(flag.clone());
            rest = tail;
        }
    }
    if chosen.is_empty() {
        return Err(USAGE.to_owned());
    }
    super::publish_release(&passed, Some(&chosen))?;
    println!(
        "Serve that folder at the base address, then name <base address>/{} on the reader \
         with kobo private-store set.",
        kobo_app_store::CATALOG_FILE
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn folder(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cobalt-private-store-cli-{}-{name}",
            std::process::id()
        ));
        let _ignored = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("folder");
        root
    }

    #[test]
    fn keygen_writes_a_seed_once_and_set_reads_it_back() {
        let root = folder("keygen");
        let seed = root.join("seed");
        let seed_text = seed.to_str().expect("path");
        command(&strings(&["keygen", "--seed", seed_text])).expect("keygen");
        assert!(command(&strings(&["keygen", "--seed", seed_text])).is_err());
        let bytes = super::super::read_signing_seed(&seed).expect("seed reads back");
        let public = kobo_app_store::derive_public_key(&bytes).expect("public key");

        let reader = root.join("reader");
        let reader_text = reader.to_str().expect("path");
        command(&strings(&[
            "set",
            "--url",
            "https://apps.example.net/own/cobalt-app-catalog-v2.json",
            "--seed",
            seed_text,
            "--root",
            reader_text,
        ]))
        .expect("set");
        let named = kobod::app_store::private::source(&reader).expect("source");
        assert_eq!(named.key, public);
        assert_eq!(
            named.url.as_deref(),
            Some("https://apps.example.net/own/cobalt-app-catalog-v2.json")
        );
        command(&strings(&["show", "--root", reader_text])).expect("show");

        command(&strings(&["remove", "--root", reader_text])).expect("remove");
        let kept = kobod::app_store::private::source(&reader).expect("key kept");
        assert_eq!((kept.url, kept.key), (None, public));
        command(&strings(&["remove", "--forget-key", "--root", reader_text])).expect("forget");
        assert!(kobod::app_store::private::source(&reader).is_none());
    }

    #[test]
    fn set_refuses_what_the_runtime_would_ignore() {
        let root = folder("refused");
        let reader = root.join("reader");
        let reader_text = reader.to_str().expect("path");
        let release = kobo_app_store::PUBLIC_RELEASE_KEY_HEX;
        let own = "11".repeat(32);
        for (url, key) in [
            ("http://apps.example.net/catalog.json", own.as_str()),
            ("https://apps.example.net/catalog.json?t=1", own.as_str()),
            ("https://apps.example.net/catalog.json", release),
            ("https://apps.example.net/catalog.json", "not-a-key"),
        ] {
            let refused = command(&strings(&[
                "set",
                "--url",
                url,
                "--key",
                key,
                "--root",
                reader_text,
            ]));
            assert!(refused.is_err(), "{url} {key} should be refused");
        }
        assert!(!reader.join("state").exists());
        // A target is required, and only one.
        assert!(command(&strings(&["show"])).is_err());
        assert!(command(&strings(&["show", "--root", "a", "--root", "b"])).is_err());
    }

    #[test]
    fn publish_refuses_the_release_key_and_unknown_applications() {
        assert!(publish(&strings(&["--registry", "r", "--seed", "s"])).is_err());
        assert!(publish(&strings(&["--app", "Not An Id"])).is_err());
    }
}
