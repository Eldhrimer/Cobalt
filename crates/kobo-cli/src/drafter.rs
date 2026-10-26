//! Owner-attended Drafter sign-in: check a Drafter gateway's bearer token from
//! the computer, pin it to that one gateway, and deliver both to the reader.
//! The application only ever names the credential; the token itself stays in
//! the runtime's store, exactly as if it had been typed onto the device.

use std::fs;
use std::time::Duration;

const CREDENTIAL: &str = "wu-drafter";
const DEVICE_STATE: &str = "/mnt/onboard/.adds/cobalt/state/drafter";
const GATEWAY_KEY: &str = "gateway";
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(60);
const USAGE: &str =
    "usage: kobo drafter login --gateway URL --token-file PATH (--sim | --device IP)\n\
                     \n\
                     Signs Drafter in to a gateway. The token file holds the\n\
                     gateway's bearer token; it is checked against the gateway\n\
                     (GET /sections) before anything is installed, pinned to that\n\
                     one gateway, and delivered to the reader's credential store,\n\
                     where only the runtime reads it. The gateway address is\n\
                     saved into Drafter's state so the app opens the book directly.";

enum Target {
    Device(String),
    Sim,
}

pub fn command(arguments: &[String]) -> Result<(), String> {
    if super::wants_help(arguments) {
        return super::print_command_help(USAGE);
    }
    match arguments.first().map(String::as_str) {
        Some("login") => login(&arguments[1..]),
        _ => Err(USAGE.to_owned()),
    }
}

struct Options {
    gateway: String,
    token: String,
    target: Target,
}

fn parse_options(arguments: &[String]) -> Result<Options, String> {
    let mut gateway = None;
    let mut token_file = None;
    let mut target = None;
    let mut rest = arguments;
    while let Some((flag, tail)) = rest.split_first() {
        match flag.as_str() {
            "--sim" => {
                if target.is_some() {
                    return Err(USAGE.to_owned());
                }
                target = Some(Target::Sim);
                rest = tail;
            }
            flag if super::is_device_flag(flag) => {
                if target.is_some() {
                    return Err(USAGE.to_owned());
                }
                let (ip, tail) = tail.split_first().ok_or_else(|| USAGE.to_owned())?;
                if !super::valid_device_host(ip) {
                    return Err("device host contains unsupported characters".to_owned());
                }
                target = Some(Target::Device(ip.clone()));
                rest = tail;
            }
            "--gateway" | "--token-file" => {
                let (value, tail) = tail.split_first().ok_or_else(|| USAGE.to_owned())?;
                match flag.as_str() {
                    "--gateway" => gateway = Some(value.clone()),
                    _ => token_file = Some(value.clone()),
                }
                rest = tail;
            }
            _ => return Err(USAGE.to_owned()),
        }
    }
    let target = target.ok_or_else(|| USAGE.to_owned())?;
    let gateway = gateway.ok_or_else(|| USAGE.to_owned())?;
    if !kobo_sdk::permissions::credentials::servers::valid_server(&gateway) {
        return Err("the gateway must be a valid HTTPS server address".to_owned());
    }
    let token_file = token_file.ok_or_else(|| USAGE.to_owned())?;
    let metadata =
        fs::symlink_metadata(&token_file).map_err(|error| format!("read {token_file}: {error}"))?;
    if !metadata.file_type().is_file() || metadata.len() > 4096 {
        return Err("the token file must be a regular file no larger than 4 KiB".into());
    }
    let token = fs::read_to_string(&token_file)
        .map_err(|error| format!("read {token_file}: {error}"))?
        .trim()
        .to_owned();
    if token.is_empty()
        || token.len() > kobo_protocol::MAX_APP_SECRET_BYTES
        || token.chars().any(char::is_control)
    {
        return Err(format!(
            "{token_file} holds no usable token (maximum 512 bytes, no control characters)"
        ));
    }
    Ok(Options {
        gateway: gateway.trim_end_matches('/').to_owned(),
        token,
        target,
    })
}

/// The credential's file format, shared with the runtime: a version line, the
/// one server the token may ever be sent to, then the token itself.
fn credential_value(gateway: &str, token: &str) -> String {
    format!("cobalt-server-account-v1\n{gateway}\n{token}")
}

fn login(arguments: &[String]) -> Result<(), String> {
    let Options {
        gateway,
        token,
        target,
    } = parse_options(arguments)?;

    // The simulator's own trust override, so a developer's local fixture
    // gateway can be logged in to exactly the way the application will use it.
    if let Some(directory) = std::env::var_os("KOBO_SIM_TRUST_DIR") {
        let _ = kobo_net::trust_owner_roots_from_dir(std::path::Path::new(&directory));
    }

    // A token that does not open the book must never reach the reader: the
    // application would install it and then fail on the owner's hands.
    let authorization = format!("Bearer {token}");
    kobo_net::fetch_from(
        &format!("{gateway}/sections"),
        0,
        16 * 1024,
        Some(("Authorization", &authorization)),
        &[],
    )
    .map_err(|error| super::post::login_error("Drafter", &gateway, error))?;

    let credential = credential_value(&gateway, &token);
    match target {
        Target::Sim => {
            let secrets = std::env::temp_dir()
                .join("cobalt-sim-secrets")
                .join("apps")
                .join("drafter")
                .join("servers");
            fs::create_dir_all(&secrets)
                .map_err(|error| format!("create {}: {error}", secrets.display()))?;
            let credential_path = secrets.join(CREDENTIAL);
            fs::write(&credential_path, &credential)
                .map_err(|error| format!("write {}: {error}", credential_path.display()))?;
            let state = std::env::temp_dir()
                .join("cobalt-sim-state")
                .join("drafter");
            fs::create_dir_all(&state)
                .map_err(|error| format!("create {}: {error}", state.display()))?;
            let gateway_path = state.join(GATEWAY_KEY);
            fs::write(&gateway_path, &gateway)
                .map_err(|error| format!("write {}: {error}", gateway_path.display()))?;
            println!(
                "Signed Drafter in to {gateway}; the simulator picks up the credential on its next start."
            );
        }
        Target::Device(host) => {
            let install = credential_install_script(&credential);
            let encoded = super::base64_encode(gateway.as_bytes());
            let script = format!(
                "{install}\
                 set -eu\n\
                 root='{DEVICE_STATE}'\n\
                 mkdir -p \"$root\"\n\
                 chmod 700 \"$root\"\n\
                 partial=\"$root/.{GATEWAY_KEY}.writing\"\n\
                 base64 -d > \"$partial\" <<'KOBO_DRAFTER_GATEWAY'\n\
                 {encoded}\n\
                 KOBO_DRAFTER_GATEWAY\n\
                 chmod 600 \"$partial\"\n\
                 mv -f \"$partial\" \"$root/{GATEWAY_KEY}\"\n\
                 sync\n"
            );
            remote(&host, &script)?;
            println!(
                "Signed Drafter in to {gateway}; Drafter on {host} picks it up on its next start."
            );
        }
    }
    Ok(())
}

/// Stage the account alongside its final app-scoped path. A failed decoder or
/// interrupted transfer must leave the previous account intact. Each transfer
/// owns a unique private staging file, so stale or concurrent files cannot
/// block sign-in or be removed by another transfer.
fn credential_install_script(credential: &str) -> String {
    let encoded = super::base64_encode(credential.as_bytes());
    format!(
        r#"set -eu
umask 077
root=/mnt/onboard/.adds/cobalt
for directory in /mnt /mnt/onboard /mnt/onboard/.adds "$root" "$root/secrets" "$root/secrets/apps" "$root/secrets/apps/drafter" "$root/secrets/apps/drafter/servers"; do
    test ! -L "$directory" || exit 1
    if test ! -d "$directory"; then mkdir "$directory"; fi
done
for directory in "$root/secrets" "$root/secrets/apps" "$root/secrets/apps/drafter" "$root/secrets/apps/drafter/servers"; do
    chmod 700 "$directory"
done
cd "$root/secrets/apps/drafter/servers"
test ! -d '{CREDENTIAL}' || exit 1
partial=$(mktemp './.{CREDENTIAL}.writing.XXXXXX') || exit 1
trap 'rm -f "$partial"' EXIT
trap 'exit 1' HUP INT TERM
base64 -d > "$partial" <<'COBALT_DRAFTER_ACCOUNT'
{encoded}
COBALT_DRAFTER_ACCOUNT
chmod 600 "$partial"
mv -f "$partial" '{CREDENTIAL}'
trap - EXIT HUP INT TERM
"#
    )
}

fn remote(host: &str, script: &str) -> Result<(), String> {
    let output = super::run_remote_shell(&format!("root@{host}"), script, TRANSFER_TIMEOUT)
        .map_err(super::unreachable_device)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(super::unreachable_if_ssh_gave_up(
            super::remote_shell_error(
                format!(
                    "Drafter sign-in delivery to {host} exited with {}",
                    output.status
                ),
                &output.stdout,
                &output.stderr,
            ),
            &output,
        ))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn flag_like_hosts_fail_before_input_or_network() {
        let base = [
            "--gateway",
            "https://drafter.example",
            "--token-file",
            "/nonexistent",
        ]
        .map(str::to_owned)
        .to_vec();
        for flag in ["--device", "-s"] {
            for host in ["--sim", "--device", "-s", "-reader", ""] {
                for first in [false, true] {
                    let mut arguments = base.clone();
                    let index = if first { 0 } else { arguments.len() };
                    arguments.splice(index..index, [flag.to_owned(), host.to_owned()]);
                    let error = parse_options(&arguments)
                        .err()
                        .expect("invalid host rejected");
                    assert!(error.contains("device host"), "{arguments:?}: {error}");
                }
            }
        }
    }

    #[test]
    fn gateways_reject_unusable_runtime_addresses_before_reading_tokens() {
        for gateway in [
            "https://",
            "http://example.com",
            "https://user@example.com",
            "https://example.com/?token=x",
            "https://example.com/#x",
            "https://example.com/../x",
            "https://example.com/%2e/x",
            "https://example.com/%0a",
            "https://example.com/\n",
        ] {
            let arguments = [
                "--sim",
                "--gateway",
                gateway,
                "--token-file",
                "/nonexistent",
            ]
            .map(str::to_owned);
            assert!(parse_options(&arguments)
                .err()
                .unwrap()
                .contains("valid HTTPS"));
        }
    }

    #[test]
    fn device_account_install_is_private_atomic_and_app_scoped() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = std::env::temp_dir().join(format!("drafter-install-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let record = credential_value("https://drafter.example", "fixture-token");
        let script = credential_install_script(&record)
            .replace("/mnt/onboard/.adds/cobalt", root.to_str().unwrap())
            .replace("/mnt /mnt/onboard /mnt/onboard/.adds ", "");
        let run = |script: &str| {
            std::process::Command::new("sh")
                .args(["-c", script])
                .output()
                .unwrap()
        };
        assert!(run(&script).status.success());
        let servers = root.join("secrets/apps/drafter/servers");
        let account = servers.join(CREDENTIAL);
        assert_eq!(fs::read_to_string(&account).unwrap(), record);
        assert_eq!(
            fs::metadata(&account).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for directory in [
            "secrets",
            "secrets/apps",
            "secrets/apps/drafter",
            "secrets/apps/drafter/servers",
        ] {
            assert_eq!(
                fs::metadata(root.join(directory))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        assert!(!root.join("secrets/wu-drafter").exists());
        let broken = script.replace("base64 -d", "false");
        assert!(!run(&broken).status.success());
        assert_eq!(fs::read_to_string(&account).unwrap(), record);
        let interrupted = script.replace("base64 -d", "kill -TERM $$\nbase64 -d");
        assert!(!run(&interrupted).status.success());
        assert_eq!(fs::read_to_string(&account).unwrap(), record);
        assert_eq!(fs::read_dir(&servers).unwrap().count(), 1);
        fs::write(servers.join(".wu-drafter.writing"), "occupied").unwrap();
        fs::write(
            servers.join(".wu-drafter.writing.other"),
            "another transfer",
        )
        .unwrap();
        assert!(run(&script).status.success());
        assert_eq!(
            fs::read_to_string(servers.join(".wu-drafter.writing")).unwrap(),
            "occupied"
        );
        assert_eq!(fs::read_to_string(&account).unwrap(), record);
        assert_eq!(
            fs::read_dir(&servers).unwrap().count(),
            3,
            "own staging cleaned up without removing another transfer"
        );
        assert_eq!(
            fs::read_to_string(servers.join(".wu-drafter.writing.other")).unwrap(),
            "another transfer"
        );
        fs::rename(&servers, root.join("redirected")).unwrap();
        symlink(root.join("redirected"), &servers).unwrap();
        assert!(!run(&script).status.success());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tokens_must_fit_the_runtime_record() {
        let file = std::env::temp_dir().join(format!("drafter-token-{}", std::process::id()));
        let arguments = [
            "--sim",
            "--gateway",
            "https://drafter.example",
            "--token-file",
            file.to_str().unwrap(),
        ]
        .map(str::to_owned);
        for token in ["x".repeat(513), "a\nb".to_owned(), "a\u{7f}b".to_owned()] {
            fs::write(&file, token).unwrap();
            assert!(parse_options(&arguments).is_err());
        }
        fs::write(&file, "x".repeat(512)).unwrap();
        assert!(parse_options(&arguments).is_ok());
        fs::remove_file(file).unwrap();
    }
    #[test]
    fn ambiguous_destinations_fail_before_input_is_read() {
        for flags in [
            &["--sim", "--sim"][..],
            &["--sim", "--device", "fixture"],
            &["--device", "fixture", "--sim"],
            &["--device", "fixture", "--device", "fixture"],
        ] {
            let mut arguments: Vec<String> = [
                "--gateway",
                "https://drafter.example",
                "--token-file",
                "/nonexistent",
            ]
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
            arguments.extend(flags.iter().map(|value| (*value).to_owned()));
            assert_eq!(parse_options(&arguments).err().unwrap(), USAGE);
        }
    }

    use super::*;

    #[test]
    fn credential_value_pins_the_gateway() {
        assert_eq!(
            credential_value("https://drafter.example", "token-1"),
            "cobalt-server-account-v1\nhttps://drafter.example\ntoken-1"
        );
    }

    #[test]
    fn options_require_https_and_a_token() {
        let arguments = |gateway: &str| {
            vec![
                "login".to_owned(),
                "--gateway".to_owned(),
                gateway.to_owned(),
                "--token-file".to_owned(),
                "/nonexistent".to_owned(),
                "--sim".to_owned(),
            ]
        };
        assert!(parse_options(&arguments("http://drafter.example")).is_err());
        // The missing token file is reported before any network use.
        assert!(parse_options(&arguments("https://drafter.example")).is_err());
    }
}
