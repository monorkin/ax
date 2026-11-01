//! The real binary against a throwaway home, with `claude` and `codex` faked
//! on the PATH and usage readings kept from "just now", so nothing here
//! reaches the network or a real login.

use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT_WORLD: AtomicU32 = AtomicU32::new(0);
const FAR_FUTURE: i64 = 4_102_444_800;

struct World {
    root: PathBuf,
}

impl World {
    fn new() -> World {
        let root = std::env::temp_dir().join(format!("ax-test-{}-{}", std::process::id(), NEXT_WORLD.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        for program in ["claude", "codex"] {
            let path = root.join("bin").join(program);
            fs::write(&path, "#!/bin/sh\necho \"$0 CLAUDE_CONFIG_DIR=$CLAUDE_CONFIG_DIR CODEX_HOME=$CODEX_HOME $*\"\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        World { root }
    }

    fn data(&self) -> PathBuf {
        self.root.join(".local/share/ax")
    }

    fn ax(&self, args: &[&str]) -> Output {
        self.ax_in(&self.root, args)
    }

    fn ax_in(&self, directory: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ax"))
            .args(args)
            .current_dir(directory)
            .env("HOME", &self.root)
            .env("PATH", format!("{}:/usr/bin:/bin", self.root.join("bin").display()))
            .env_remove("XDG_DATA_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.ax(args);
        assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    }

    /// Stored accounts, each `(number, provider, email, percent used)`.
    fn store(&self, accounts: &[(u32, &str, &str, f64)]) {
        let mut roster = Vec::new();
        let mut usage = serde_json::Map::new();
        for (number, provider, email, used) in accounts {
            roster.push(json!({ "number": number, "provider": provider, "email": email, "organizationUuid": organization_of(*number), "added": "2026-11-01T00:00:00Z" }));
            usage.insert(number.to_string(), json!({
                "reading": { "usage": { "five_hour": { "utilization": used }, "seven_day": null }, "taken_at": 1_790_000_000 },
                "ask_again_at": FAR_FUTURE,
            }));
            let credentials = if *provider == "codex" { codex_auth(email, *number, "stored") } else { claude_credentials(*number, "stored") };
            write(&self.data().join(format!("credentials/account-{number}.json")), &credentials);
            write(&self.data().join(format!("configs/account-{number}.json")), &claude_config(email, *number));
        }
        write(&self.data().join("accounts.json"), &json!({ "accounts": roster }));
        write(&self.data().join("usage.json"), &Value::Object(usage));
    }

    fn log_claude_into(&self, email: &str, number: u32) {
        write(&self.root.join(".claude.json"), &claude_config(email, number));
        write(&self.root.join(".claude/.credentials.json"), &claude_credentials(number, "default"));
    }

    fn pool_folder(&self, pool: &str, provider: &str) -> PathBuf {
        self.data().join("pools").join(pool).join(provider)
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn organization_of(number: u32) -> String {
    format!("org-{number}")
}

fn claude_config(email: &str, number: u32) -> Value {
    json!({ "oauthAccount": { "emailAddress": email, "accountUuid": "", "organizationUuid": organization_of(number), "organizationName": "Example" } })
}

fn claude_credentials(number: u32, generation: &str) -> Value {
    json!({ "claudeAiOauth": { "accessToken": format!("{generation}-{number}"), "expiresAt": FAR_FUTURE * 1000 } })
}

fn codex_auth(email: &str, number: u32, generation: &str) -> Value {
    let claims = json!({
        "email": email,
        "exp": FAR_FUTURE,
        "https://api.openai.com/auth": { "chatgpt_account_id": organization_of(number), "chatgpt_plan_type": "team", "chatgpt_user_id": "user-1" },
    });
    let token = format!("x.{}.y", base64_url(claims.to_string().as_bytes()));
    json!({
        "auth_mode": "chatgpt",
        "tokens": { "id_token": token, "access_token": token, "refresh_token": format!("{generation}-{number}"), "account_id": organization_of(number) },
    })
}

fn base64_url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let group = chunk.iter().fold(0u32, |sum, it| (sum << 8) | u32::from(*it)) << (8 * (3 - chunk.len()));
        for index in 0..=chunk.len() {
            text.push(ALPHABET[(group >> (18 - 6 * index) & 63) as usize] as char);
        }
    }
    text
}

fn write(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value.to_string()).unwrap();
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn claude_token_in(folder: &Path) -> String {
    read(&folder.join(".credentials.json"))["claudeAiOauth"]["accessToken"].as_str().unwrap().to_string()
}

#[test]
fn running_a_pool_uses_its_own_login_on_the_emptiest_member() {
    let world = World::new();
    world.store(&[(1, "claude", "default@example.com", 5.0), (2, "claude", "busy@example.com", 70.0), (3, "claude", "free@example.com", 20.0), (4, "claude", "outside@example.com", 0.0)]);
    world.log_claude_into("default@example.com", 1);
    world.ok(&["pool", "add", "work", "2", "3"]);

    let output = world.ok(&["run", "--pool", "work", "--", "--hello"]);

    let folder = world.pool_folder("work", "claude");
    assert!(output.contains(&format!("CLAUDE_CONFIG_DIR={}", folder.display())), "{output}");
    assert!(output.ends_with("--hello\n"), "{output}");
    assert_eq!(claude_token_in(&folder), "stored-3");
    assert_eq!(read(&folder.join(".claude.json"))["hasCompletedOnboarding"], true);
    assert_eq!(claude_token_in(&world.root.join(".claude")), "default-1", "the default login is left alone");
}

#[test]
fn auto_switching_a_pool_rotates_only_its_login_and_only_within_it() {
    let world = World::new();
    world.store(&[(1, "claude", "default@example.com", 95.0), (2, "claude", "busy@example.com", 95.0), (3, "claude", "free@example.com", 20.0), (4, "claude", "outside@example.com", 0.0)]);
    world.log_claude_into("default@example.com", 1);
    world.ok(&["pool", "add", "work", "2", "3"]);

    let folder = world.pool_folder("work", "claude");
    write(&folder.join(".claude.json"), &claude_config("busy@example.com", 2));
    write(&folder.join(".credentials.json"), &claude_credentials(2, "rotated"));

    let output = world.ok(&["auto-switch", "--pool", "work", "--once"]);

    assert!(output.contains("switched to free@example.com"), "{output}");
    assert_eq!(claude_token_in(&folder), "stored-3");
    assert_eq!(read(&folder.join(".claude.json"))["oauthAccount"]["emailAddress"], "free@example.com");
    assert_eq!(claude_token_in(&world.root.join(".claude")), "default-1", "a pool's rotation leaves the default login on its account");
    let backed_up = read(&world.data().join("credentials/account-2.json"));
    assert_eq!(backed_up["claudeAiOauth"]["accessToken"], "rotated-2", "the outgoing member's live tokens are kept");
}

#[test]
fn pools_keep_their_cooldowns_apart() {
    let world = World::new();
    world.store(&[(1, "claude", "a@example.com", 95.0), (2, "claude", "b@example.com", 20.0), (3, "claude", "c@example.com", 95.0), (4, "claude", "d@example.com", 20.0)]);
    world.ok(&["pool", "add", "work", "1", "2"]);
    world.ok(&["pool", "add", "private", "3", "4"]);
    for (pool, email, number) in [("work", "a@example.com", 1), ("private", "c@example.com", 3)] {
        let folder = world.pool_folder(pool, "claude");
        write(&folder.join(".claude.json"), &claude_config(email, number));
        write(&folder.join(".credentials.json"), &claude_credentials(number, "live"));
    }

    assert!(world.ok(&["auto-switch", "--pool", "work", "--once"]).contains("switched to b@example.com"));
    assert!(world.ok(&["auto-switch", "--pool", "private", "--once"]).contains("switched to d@example.com"), "work's switch doesn't hold private back");
}

#[test]
fn an_account_live_in_another_login_is_never_rotated_into() {
    let world = World::new();
    world.store(&[(1, "claude", "default@example.com", 5.0), (2, "claude", "busy@example.com", 95.0)]);
    world.log_claude_into("default@example.com", 1);
    world.ok(&["pool", "add", "work", "1", "2"]);
    let folder = world.pool_folder("work", "claude");
    write(&folder.join(".claude.json"), &claude_config("busy@example.com", 2));
    write(&folder.join(".credentials.json"), &claude_credentials(2, "live"));

    let output = world.ok(&["auto-switch", "--pool", "work", "--once"]);

    assert!(output.contains("no other account has meaningfully more headroom"), "{output}");
    assert_eq!(claude_token_in(&folder), "live-2");
}

#[test]
fn a_pool_with_every_member_live_elsewhere_refuses_to_start() {
    let world = World::new();
    world.store(&[(1, "claude", "default@example.com", 5.0)]);
    world.log_claude_into("default@example.com", 1);
    world.ok(&["pool", "add", "work", "1"]);

    let output = world.ax(&["run", "--pool", "work"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("live in another login"));
}

#[test]
fn codex_accounts_are_added_switched_and_run_apart_from_claude() {
    let world = World::new();
    write(&world.root.join(".codex/auth.json"), &codex_auth("anna@example.com", 7, "live"));
    write(&world.root.join(".codex/config.toml"), &json!("shared"));
    world.log_claude_into("claude@example.com", 9);

    let added = world.ok(&["account", "add", "codex", "--alias", "anna"]);
    assert!(added.contains("Added account 1: anna@example.com"), "{added}");
    let stored = read(&world.data().join("accounts.json"));
    assert_eq!(stored["accounts"][0]["provider"], "codex");
    assert_eq!(stored["accounts"][0]["organizationUuid"], "org-7");
    assert_eq!(stored["accounts"][0]["organizationName"], "ChatGPT team");

    write(&world.data().join("credentials/account-2.json"), &codex_auth("bea@example.com", 8, "stored"));
    let mut roster = stored.clone();
    roster["accounts"].as_array_mut().unwrap().push(json!({ "number": 2, "provider": "codex", "email": "bea@example.com", "organizationUuid": "org-8", "added": "2026-11-01T00:00:00Z" }));
    write(&world.data().join("accounts.json"), &roster);

    write(&world.root.join(".codex/auth.json"), &codex_auth("anna@example.com", 7, "rotated"));
    assert!(world.ok(&["switch", "bea@example.com"]).contains("Switched to bea@example.com"));
    assert_eq!(read(&world.root.join(".codex/auth.json"))["tokens"]["refresh_token"], "stored-8");
    assert_eq!(read(&world.data().join("credentials/account-1.json"))["tokens"]["refresh_token"], "rotated-7", "the outgoing login is backed up");
    assert_eq!(claude_token_in(&world.root.join(".claude")), "default-9", "Claude Code's login isn't involved");
    assert!(world.ok(&["switch", "bea@example.com"]).contains("Already on"));

    let output = world.ok(&["run", "--account", "anna", "--", "--hello"]);
    assert!(output.contains("/codex "), "{output}");
    assert!(output.contains("CODEX_HOME=") && output.contains("sessions/1-anna_example.com"), "{output}");
    let profile = world.data().join("sessions/1-anna_example.com");
    assert_eq!(read(&profile.join("auth.json"))["tokens"]["refresh_token"], "rotated-7");
    assert!(profile.join("config.toml").is_symlink());
}

#[test]
fn a_mixed_pool_keeps_each_tool_to_its_own_accounts() {
    let world = World::new();
    world.store(&[(1, "claude", "a@example.com", 10.0), (2, "codex", "a@example.com", 30.0), (3, "codex", "b@example.com", 5.0)]);
    world.ok(&["pool", "add", "work", "1", "2", "3"]);

    assert!(world.ok(&["run", "--pool", "work"]).contains("/claude "));
    let codex = world.ok(&["run", "codex", "--pool", "work"]);
    assert!(codex.contains("/codex ") && codex.contains(&format!("CODEX_HOME={}", world.pool_folder("work", "codex").display())), "{codex}");
    assert_eq!(read(&world.pool_folder("work", "codex").join("auth.json"))["tokens"]["refresh_token"], "stored-3");
    assert_eq!(claude_token_in(&world.pool_folder("work", "claude")), "stored-1");

    let listed = world.ok(&["pool", "list"]);
    assert!(listed.contains("* 3. b@example.com [codex]"), "{listed}");
    assert!(listed.contains("  2. a@example.com [codex]"), "{listed}");
}

#[test]
fn pools_are_kept_in_step_with_the_accounts() {
    let world = World::new();
    world.store(&[(1, "claude", "a@example.com", 10.0), (2, "claude", "b@example.com", 30.0), (3, "claude", "c@example.com", 30.0)]);
    world.ok(&["pool", "add", "work", "1", "b@example.com", "1"]);
    assert_eq!(read(&world.data().join("accounts.json"))["pools"]["work"], json!([1, 2]));

    assert!(!world.ax(&["pool", "add", "work", "99"]).status.success());
    assert!(!world.ax(&["run", "--pool", "nope"]).status.success());
    assert!(!world.ax(&["run", "--pool", "work", "--account", "1"]).status.success());

    world.ok(&["account", "remove", "1"]);
    assert_eq!(read(&world.data().join("accounts.json"))["pools"]["work"], json!([2]));
    world.ok(&["account", "remove", "2"]);
    assert!(read(&world.data().join("accounts.json"))["pools"].is_null(), "a pool with nobody left is gone");

    world.ok(&["pool", "add", "work", "3"]);
    world.ok(&["pool", "remove", "work"]);
    assert!(world.ok(&["pool", "list"]).contains("No pools yet"));
}

#[test]
fn a_directory_maps_one_account_per_tool() {
    let world = World::new();
    world.store(&[(1, "claude", "private@example.com", 0.0), (2, "codex", "private@example.com", 0.0), (3, "claude", "client@example.com", 0.0)]);
    let private = world.root.join("Private");
    let client = private.join("client");
    fs::create_dir_all(&client).unwrap();
    let run_in = |directory: &Path, args: &[&str]| String::from_utf8(world.ax_in(directory, args).stdout).unwrap();

    world.ok(&["map", private.to_str().unwrap(), "--to", "1"]);
    world.ok(&["map", private.to_str().unwrap(), "--to", "2"]);
    world.ok(&["map", client.to_str().unwrap(), "--to", "3"]);

    let claude = run_in(&private, &["run"]);
    assert!(claude.contains("/claude ") && claude.contains("sessions/1-private_example.com"), "{claude}");
    let codex = run_in(&private, &["run", "codex"]);
    assert!(codex.contains("/codex ") && codex.contains("sessions/2-private_example.com"), "{codex}");

    assert!(run_in(&client, &["run"]).contains("sessions/3-client_example.com"), "a subdirectory changes one tool's account");
    assert!(run_in(&client, &["run", "codex"]).contains("sessions/2-private_example.com"), "and inherits the other's");

    world.ok(&["mapping", "remove", private.to_str().unwrap(), "codex"]);
    let unmapped = run_in(&private, &["run", "codex"]);
    assert!(unmapped.contains("/codex ") && !unmapped.contains("sessions/"), "{unmapped}");
    assert!(run_in(&private, &["run"]).contains("sessions/1-private_example.com"), "the other tool's mapping stays");

    world.ok(&["map", private.to_str().unwrap(), "--to", "2"]);
    world.ok(&["mapping", "remove", private.to_str().unwrap()]);
    let listed = world.ok(&["mapping", "list"]);
    assert_eq!(listed.lines().count(), 1, "{listed}");
    assert!(listed.ends_with("client → client@example.com [claude]\n"), "{listed}");
}

#[test]
fn a_mapping_saved_before_there_were_tools_is_a_claude_mapping() {
    let world = World::new();
    world.store(&[(1, "claude", "a@example.com", 0.0)]);
    let directory = world.root.join("project");
    fs::create_dir_all(&directory).unwrap();
    write(&world.data().join("mappings.json"), &json!({
        "schemaVersion": 1,
        "mappings": { directory.canonicalize().unwrap().to_str().unwrap(): { "email": "a@example.com", "organizationUuid": "org-1", "added": "2026-01-01T00:00:00Z" } },
    }));

    let output = String::from_utf8(world.ax_in(&directory, &["run"]).stdout).unwrap();
    assert!(output.contains("sessions/1-a_example.com"), "{output}");
}
