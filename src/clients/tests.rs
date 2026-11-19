//! Every client's format, written and read back in a temporary home folder.

use super::*;

fn env(home: &Path) -> Env {
    Env {
        home: home.to_path_buf(),
        xdg_config: home.join(".config"),
        app_config: home.join("AppConfig"),
        local_data: home.join("LocalData"),
        claude_config_dir: None,
        codex_home: None,
        hermes_home: None,
        gemini_home: None,
        qwen_home: None,
        copilot_home: None,
        goose_root: None,
        search: vec![home.join("bin")],
    }
}

fn launch() -> Launch {
    Launch { command: "/opt/zone/bin/zone".to_string(), args: vec!["mcp".to_string()], env: Vec::new() }
}

fn with_home() -> Launch {
    Launch { env: vec![("ZONE_HOME".to_string(), "/tmp/zone home".to_string())], ..launch() }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn json_at(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not strict JSON ({e}):\n{text}", path.display()))
}

fn only(client: Client, env: &Env) -> Target {
    let targets = client.targets(env);
    assert_eq!(targets.len(), 1, "{client:?}");
    targets.into_iter().next().unwrap()
}

/// Registers, checks what was written with `check`, registers again (no change), then
/// removes and expects the file exactly as it was.
fn round_trip(client: Client, env: &Env, original: Option<&str>, check: impl Fn(&str)) {
    let target = only(client, env);
    if let Some(text) = original {
        write(&target.path, text);
    }
    let mut state = State::default();
    assert_eq!(register(&target, &with_home(), env, &mut state, Mode::Explicit), Outcome::Registered, "{client:?}");
    let text = std::fs::read_to_string(&target.path).unwrap();
    check(&text);
    assert_eq!(read_entry(&target, env), Found::Present(with_home()), "{client:?} reads back what it wrote:\n{text}");
    assert_eq!(register(&target, &with_home(), env, &mut state, Mode::Explicit), Outcome::Already, "{client:?}");
    assert!(state.registered.contains(&target.id()));
    if let Some(text) = original {
        let backup = PathBuf::from(format!("{}.before-zone", target.path.display()));
        assert_eq!(std::fs::read_to_string(backup).unwrap(), text, "{client:?} keeps the original");
    }
    assert_eq!(unregister(&target, env, &mut state, true), Outcome::Removed, "{client:?}");
    if let Some(text) = original {
        let now = std::fs::read_to_string(&target.path).unwrap();
        if now != text {
            // Only allowed difference: the (now empty) object zone's entry went into, when the
            // file did not have it before.
            let (Ok(Some(Value::Object(mut after))), Ok(Some(Value::Object(before)))) = (jsonc::get(&now, &[]), jsonc::get(text, &[])) else {
                panic!("{client:?} is not back to what it was:\n{now}");
            };
            after.retain(|key, value| before.contains_key(key) || value != &json!({}));
            assert_eq!(after, before, "{client:?} is back to what it was");
        }
    }
    assert_eq!(read_entry(&target, env), Found::Absent);
    assert!(state.removed.contains(&target.id()));
    assert_eq!(unregister(&target, env, &mut state, true), Outcome::NotThere);
}

#[test]
fn plain_json_clients() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path());
    let existing = "{\n  \"theme\": \"dark\",\n  \"mcpServers\": {\n    \"files\": {\n      \"command\": \"npx\",\n      \"args\": [\"-y\", \"files\"]\n    }\n  },\n  \"last\": true\n}\n";
    for client in [Client::ClaudeDesktop, Client::Cursor, Client::Gemini, Client::Qwen, Client::Windsurf, Client::Kiro, Client::LmStudio, Client::Roo] {
        round_trip(client, &env, Some(existing), |text| {
            let value: Value = serde_json::from_str(text).unwrap();
            assert_eq!(value["mcpServers"]["zone"], json!({ "command": "/opt/zone/bin/zone", "args": ["mcp"], "env": { "ZONE_HOME": "/tmp/zone home" } }));
            assert_eq!(value["mcpServers"]["files"]["args"], json!(["-y", "files"]));
            assert_eq!((value["theme"].as_str(), value["last"].as_bool()), (Some("dark"), Some(true)));
        });
    }
    // A missing file (and folder) is created, with nothing else in it.
    let empty = tempfile::tempdir().unwrap();
    round_trip(Client::Gemini, &super::tests::env(empty.path()), None, |text| {
        let value: Value = serde_json::from_str(text).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 1);
    });
    // Without ZONE_HOME there is no env at all.
    let target = only(Client::Cursor, &env);
    register(&target, &launch(), &env, &mut State::default(), Mode::Explicit);
    assert_eq!(json_at(&target.path)["mcpServers"]["zone"], json!({ "command": "/opt/zone/bin/zone", "args": ["mcp"] }));
}

#[test]
fn clients_with_their_own_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path());
    round_trip(Client::VsCode, &env, Some("{\n  // my servers\n  \"servers\": {},\n  \"inputs\": []\n}\n"), |text| {
        assert!(text.contains("// my servers"));
        let zone = jsonc::get(text, &["servers", "zone"]).unwrap().unwrap();
        assert_eq!(zone, json!({ "type": "stdio", "command": "/opt/zone/bin/zone", "args": ["mcp"], "env": { "ZONE_HOME": "/tmp/zone home" } }));
    });
    round_trip(Client::OpenCode, &env, Some("{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  \"mcp\": {}\n}\n"), |text| {
        let value: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            value["mcp"]["zone"],
            json!({ "type": "local", "command": ["/opt/zone/bin/zone", "mcp"], "enabled": true, "environment": { "ZONE_HOME": "/tmp/zone home" } })
        );
    });
    round_trip(Client::Zed, &env, Some("// Zed settings\n{\n  \"theme\": \"One Dark\",\n  \"context_servers\": {\n    \"other\": { \"command\": \"x\", \"args\": [] }\n  },\n}\n"), |text| {
        assert!(text.starts_with("// Zed settings\n"));
        let zone = jsonc::get(text, &["context_servers", "zone"]).unwrap().unwrap();
        assert_eq!(zone, json!({ "command": "/opt/zone/bin/zone", "args": ["mcp"], "env": { "ZONE_HOME": "/tmp/zone home" } }));
    });
    round_trip(Client::Droid, &env, Some("{\"mcpServers\":{}}"), |text| {
        assert_eq!(serde_json::from_str::<Value>(text).unwrap()["mcpServers"]["zone"]["type"], "stdio");
    });
    round_trip(Client::Copilot, &env, None, |text| {
        let zone = &serde_json::from_str::<Value>(text).unwrap()["mcpServers"]["zone"];
        assert_eq!((zone["type"].as_str(), zone["tools"].clone()), (Some("local"), json!(["*"])));
    });
    round_trip(Client::Crush, &env, Some("{\n  \"$schema\": \"https://charm.land/crush.json\"\n}\n"), |text| {
        assert_eq!(serde_json::from_str::<Value>(text).unwrap()["mcp"]["zone"]["type"], "stdio");
    });
    round_trip(Client::Amp, &env, Some("{\n  \"amp.notifications.enabled\": false\n}\n"), |text| {
        let value: Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["amp.mcpServers"]["zone"]["command"], "/opt/zone/bin/zone");
        assert_eq!(value["amp.notifications.enabled"], false);
    });
    round_trip(Client::Codex, &env, Some("model = \"gpt\"\n\n[mcp_servers.other]\ncommand = \"other\"\n"), |text| {
        assert!(text.ends_with("[mcp_servers.zone]\ncommand = \"/opt/zone/bin/zone\"\nargs = [\"mcp\"]\nenv = { ZONE_HOME = \"/tmp/zone home\" }\n"), "{text}");
    });
}

#[test]
fn yaml_clients_keep_their_comments() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path());
    let hermes = "model:\n  default: some-model\n# servers\nmcp_servers:\n  files:\n    command: npx\n    args:\n      - files\n\nvoice:\n  enabled: false\n";
    round_trip(Client::Hermes, &env, Some(hermes), |text| {
        assert!(text.contains("  files:\n    command: npx\n    args:\n      - files\n  zone:\n    command: \"/opt/zone/bin/zone\"\n    args: [\"mcp\"]\n    env:\n      ZONE_HOME: \"/tmp/zone home\"\n\nvoice:"), "{text}");
    });
    round_trip(Client::Goose, &env, Some("GOOSE_PROVIDER: openai\nextensions:\n  developer:\n    enabled: true\n    type: builtin\n"), |text| {
        let fields = yaml::get(text, "extensions", "zone").unwrap();
        assert!(fields.contains(&("cmd".to_string(), Yaml::Str("/opt/zone/bin/zone".into()))), "{text}");
        assert!(fields.contains(&("type".to_string(), Yaml::Str("stdio".into()))));
        assert!(fields.contains(&("enabled".to_string(), Yaml::Bool(true))));
        assert!(text.starts_with("GOOSE_PROVIDER: openai\nextensions:\n  developer:\n"));
    });
}

#[test]
fn claude_code_profiles_are_found_and_written_without_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = env(dir.path());
    let home = dir.path();
    write(&home.join(".claude.json"), "{\n  \"numStartups\": 3,\n  \"projects\": {}\n}\n");
    write(&home.join(".claude").join(".claude.json"), "{}");
    write(&home.join(".claude_work").join(".claude.json"), "{\"mcpServers\": {\"x\": {\"type\": \"stdio\", \"command\": \"x\", \"args\": []}}}");
    // A folder without a .claude.json is not a profile.
    std::fs::create_dir_all(home.join(".claude-server-commander")).unwrap();
    // CLAUDE_CONFIG_DIR may point anywhere.
    let elsewhere = home.join("profiles").join("team");
    std::fs::create_dir_all(&elsewhere).unwrap();
    env.claude_config_dir = Some(elsewhere.clone());

    let targets = Client::ClaudeCode.targets(&env);
    let labels: Vec<String> = targets.iter().map(|t| t.label()).collect();
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(
        labels,
        vec![
            "Claude Code (default)".to_string(),
            format!("Claude Code (~{sep}profiles{sep}team)"),
            format!("Claude Code (~{sep}.claude)"),
            format!("Claude Code (~{sep}.claude_work)"),
        ]
    );
    assert_eq!(targets[0].claude_dir, Some(None));
    assert_eq!(targets[1].claude_dir, Some(Some(elsewhere.clone())));
    assert!(Client::ClaudeCode.installed(&env));

    // No `claude` command: each profile's .claude.json is edited the way `claude mcp add` would.
    let mut state = State::default();
    assert_eq!(register_many(&[Client::ClaudeCode], &launch(), Mode::All, &env, &mut state, false), 0);
    for target in &targets {
        let entry = json_at(&target.path)["mcpServers"]["zone"].clone();
        assert_eq!(entry, json!({ "type": "stdio", "command": "/opt/zone/bin/zone", "args": ["mcp"], "env": {} }), "{}", target.label());
    }
    assert_eq!(json_at(&home.join(".claude.json"))["numStartups"], 3);
    assert_eq!(json_at(&home.join(".claude_work").join(".claude.json"))["mcpServers"]["x"]["command"], "x");

    let rows = survey(&env, &state, &launch());
    let claude: Vec<&Row> = rows.iter().filter(|r| r.target.client == Client::ClaudeCode).collect();
    assert_eq!(claude.len(), 4);
    assert!(claude.iter().all(|r| r.standing == Standing::Registered { read_only: false, scope: Some(Scope::default()) }));

    // Taking zone out of one profile leaves the others.
    assert_eq!(unregister(&targets[3], &env, &mut state, true), Outcome::Removed);
    assert!(json_at(&targets[3].path)["mcpServers"].get("zone").is_none());
    assert!(json_at(&targets[0].path)["mcpServers"].get("zone").is_some());
}

#[cfg(unix)]
#[test]
fn claude_code_uses_its_own_command_for_each_profile() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path());
    let home = dir.path();
    write(&home.join(".claude_work").join(".claude.json"), "{}");
    // A stand-in `claude` that records how it was called and writes the entry like the
    // real one: `mcp add --scope user zone -- <command> <args>`.
    let record = home.join("calls.txt");
    let script = format!(
        "#!/bin/sh\nprintf '%s|%s\\n' \"${{CLAUDE_CONFIG_DIR:-unset}}\" \"$*\" >> '{record}'\n\
         file=\"$CLAUDE_CONFIG_DIR/.claude.json\"\n\
         [ -n \"$CLAUDE_CONFIG_DIR\" ] || file='{home}/.claude.json'\n\
         if [ \"$2\" = add ]; then shift 6; printf '{{\"mcpServers\":{{\"zone\":{{\"type\":\"stdio\",\"command\":\"%s\",\"args\":[\"%s\"],\"env\":{{}}}}}}}}' \"$1\" \"$2\" > \"$file\"; fi\n\
         if [ \"$2\" = remove ]; then printf '{{\"mcpServers\":{{}}}}' > \"$file\"; fi\n",
        record = record.display(),
        home = home.display()
    );
    let fake = home.join("bin").join("claude");
    write(&fake, &script);
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut state = State::default();
    let targets = Client::ClaudeCode.targets(&env);
    assert_eq!(targets.len(), 2);
    for target in &targets {
        assert_eq!(register(target, &launch(), &env, &mut state, Mode::All), Outcome::Registered, "{}", target.label());
    }
    let calls = std::fs::read_to_string(&record).unwrap();
    let work = home.join(".claude_work");
    assert_eq!(
        calls,
        format!("unset|mcp add --scope user zone -- /opt/zone/bin/zone mcp\n{}|mcp add --scope user zone -- /opt/zone/bin/zone mcp\n", work.display())
    );
    // Already there: the command is not run again.
    assert_eq!(register(&targets[1], &launch(), &env, &mut state, Mode::All), Outcome::Already);
    assert_eq!(std::fs::read_to_string(&record).unwrap(), calls);
    // Removing goes through the command too, for that profile only.
    assert_eq!(unregister(&targets[1], &env, &mut state, true), Outcome::Removed);
    assert!(std::fs::read_to_string(&record).unwrap().ends_with(&format!("{}|mcp remove --scope user zone\n", work.display())));
    assert!(matches!(read_entry(&targets[0], &env), Found::Present(_)));
}

#[test]
fn codex_homes_and_hermes_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = env(dir.path());
    let home = dir.path();
    write(&home.join(".codex").join("config.toml"), "model = \"a\"\n");
    write(&home.join(".codex_team").join("config.toml"), "model = \"b\"\n");
    std::fs::create_dir_all(home.join(".codex-worktrees")).unwrap();
    env.codex_home = Some(home.join(".codex"));
    let codex: Vec<String> = Client::Codex.targets(&env).iter().map(|t| t.label()).collect();
    assert_eq!(codex, vec!["Codex (default)".to_string(), format!("Codex (~{}.codex_team)", std::path::MAIN_SEPARATOR)]);

    // Where Hermes keeps its files on this system (under the home folder, or on Windows under
    // the local application data).
    let hermes_home = env.hermes_dir();
    write(&hermes_home.join("config.yaml"), "mcp_servers: {}\n");
    write(&hermes_home.join("profiles").join("work").join("config.yaml"), "model: x\n");
    std::fs::create_dir_all(hermes_home.join("profiles").join("empty")).unwrap();
    let hermes = Client::Hermes.targets(&env);
    assert_eq!(hermes.iter().map(|t| t.label()).collect::<Vec<_>>(), vec!["Hermes (default)", "Hermes (profile work)"]);
    let mut state = State::default();
    assert_eq!(register_many(&[Client::Codex, Client::Hermes], &launch(), Mode::All, &env, &mut state, false), 0);
    assert!(std::fs::read_to_string(home.join(".codex_team").join("config.toml")).unwrap().contains("[mcp_servers.zone]"));
    let work = std::fs::read_to_string(hermes_home.join("profiles").join("work").join("config.yaml")).unwrap();
    assert!(work.starts_with("model: x\nmcp_servers:\n  zone:\n"), "{work}");
    let default = std::fs::read_to_string(hermes_home.join("config.yaml")).unwrap();
    assert!(default.starts_with("mcp_servers:\n  zone:\n"), "{default}");
}

#[test]
fn detection_needs_the_settings_or_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path());
    let home = dir.path();
    assert!(Client::ALL.iter().all(|c| !c.installed(&env)), "an empty home has no clients");
    // A folder that only holds shared skills does not count for Goose or Crush.
    std::fs::create_dir_all(home.join(".config").join("goose").join("skills")).unwrap();
    std::fs::create_dir_all(home.join(".config").join("crush").join("skills")).unwrap();
    assert!(!Client::Goose.installed(&env) && !Client::Crush.installed(&env));
    // A settings folder counts, and so does a command.
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    assert!(Client::Gemini.installed(&env));
    write(&home.join("bin").join("opencode"), "");
    assert!(Client::OpenCode.installed(&env));
    let found: Vec<Client> = Client::ALL.into_iter().filter(|c| c.installed(&env)).collect();
    assert_eq!(found, vec![Client::Gemini, Client::OpenCode]);
}

#[test]
fn all_skips_opt_outs_and_leaves_read_only_entries_alone() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path());
    let home = dir.path();
    std::fs::create_dir_all(home.join(".cursor")).unwrap();
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    std::fs::create_dir_all(home.join(".kiro")).unwrap();
    let mut state = State::default();
    assert_eq!(register_many(&Client::ALL, &launch(), Mode::All, &env, &mut state, true), 0);
    let cursor = only(Client::Cursor, &env);
    let gemini = only(Client::Gemini, &env);
    let kiro = only(Client::Kiro, &env);

    // Taken out with `zone mcp uninstall`: --all leaves it out.
    unregister(&cursor, &env, &mut state, true);
    assert_eq!(register(&cursor, &launch(), &env, &mut state, Mode::All), Outcome::OptedOut);
    assert_eq!(read_entry(&cursor, &env), Found::Absent);
    // Taken out by hand after zone added it: the same.
    std::fs::write(&gemini.path, "{\"mcpServers\": {}}").unwrap();
    assert_eq!(register(&gemini, &launch(), &env, &mut state, Mode::All), Outcome::OptedOut);
    assert!(state.removed.contains(&gemini.id()));
    // Read-only on purpose: left as it is.
    let read_only = Launch { args: vec!["mcp".into(), "--read-only".into()], ..launch() };
    register(&kiro, &read_only, &env, &mut state, Mode::Explicit);
    assert_eq!(register(&kiro, &launch(), &env, &mut state, Mode::All), Outcome::Already);
    assert_eq!(read_entry(&kiro, &env), Found::Present(read_only.clone()));
    // Another copy of zone: --all points it here, the daemon does not.
    let old = Launch { command: "/old/zone".into(), ..launch() };
    register(&kiro, &old, &env, &mut state, Mode::Explicit);
    assert_eq!(register(&kiro, &launch(), &env, &mut state, Mode::Automatic), Outcome::LeftAlone("/old/zone".into()));
    assert_eq!(register(&kiro, &launch(), &env, &mut state, Mode::All), Outcome::Updated);
    // Asking for one client by name puts it back.
    assert_eq!(register(&cursor, &launch(), &env, &mut state, Mode::Explicit), Outcome::Registered);
    assert!(!state.removed.contains(&cursor.id()));

    let rows = survey(&env, &state, &launch());
    let standing = |client: Client| rows.iter().find(|r| r.target.client == client).unwrap().standing.clone();
    assert_eq!(standing(Client::Cursor), Standing::Registered { read_only: false, scope: Some(Scope::default()) });
    assert_eq!(standing(Client::Gemini), Standing::OptedOut);
    assert_eq!(standing(Client::Zed), Standing::NotRegistered);
}

#[test]
fn broken_files_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path());
    let target = only(Client::Cursor, &env);
    for text in ["{ not json", "[1, 2]", "{ \"mcpServers\": 5 }"] {
        write(&target.path, text);
        let outcome = register(&target, &launch(), &env, &mut State::default(), Mode::Explicit);
        assert!(matches!(&outcome, Outcome::Failed(why) if why.contains("mcp.json")), "{outcome:?}");
        assert_eq!(std::fs::read_to_string(&target.path).unwrap(), text);
    }
    let hermes = Client::Hermes.targets(&env).remove(0);
    write(&hermes.path, "mcp_servers: {a: 1}\n");
    assert!(register(&hermes, &launch(), &env, &mut State::default(), Mode::Explicit).is_failure());
}

#[cfg(unix)]
#[test]
fn links_and_permissions_survive() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path());
    let real = dir.path().join("dotfiles").join("claude.json");
    write(&real, "{}");
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&real, dir.path().join(".claude.json")).unwrap();
    let target = Client::ClaudeCode.targets(&env).remove(0);
    assert_eq!(register(&target, &launch(), &env, &mut State::default(), Mode::Explicit), Outcome::Registered);
    assert!(std::fs::symlink_metadata(dir.path().join(".claude.json")).unwrap().file_type().is_symlink(), "still a link");
    assert_eq!(json_at(&real)["mcpServers"]["zone"]["command"], "/opt/zone/bin/zone");
    assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn client_names() {
    assert_eq!(Client::parse("Claude-Code").unwrap(), Client::ClaudeCode);
    assert_eq!(Client::parse("claude_desktop").unwrap(), Client::ClaudeDesktop);
    assert_eq!(Client::parse("VS Code").unwrap(), Client::VsCode);
    assert_eq!(Client::parse("gemini-cli").unwrap(), Client::Gemini);
    for client in Client::ALL {
        assert_eq!(Client::parse(client.name()).unwrap(), client);
    }
    assert!(Client::parse("emacs").unwrap_err().to_string().contains("claude-code, claude-desktop, codex, cursor, hermes"));
    assert_eq!(chosen(&["detected".into()], false).unwrap(), (Client::ALL.to_vec(), true));
    assert_eq!(chosen(&["zed".into(), "zed".into(), "cursor".into()], false).unwrap(), (vec![Client::Zed, Client::Cursor], false));
    assert!(chosen(&[], false).is_err());
    assert_eq!(claude_code_manual(&launch(), None), "claude mcp add --scope user zone -- \"/opt/zone/bin/zone\" mcp");
}

#[test]
fn settings_moved_by_environment_variables_and_pointers() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let mut env = env(home);
    env.gemini_home = Some(home.join("g"));
    env.qwen_home = Some(home.join("q"));
    env.copilot_home = Some(home.join("c"));
    env.goose_root = Some(home.join("goose-root"));
    env.hermes_home = Some(home.join("h"));
    assert_eq!(only(Client::Gemini, &env).path, home.join("g").join(".gemini").join("settings.json"));
    assert_eq!(only(Client::Qwen, &env).path, home.join("q").join("settings.json"));
    assert_eq!(only(Client::Copilot, &env).path, home.join("c").join("mcp-config.json"));
    assert_eq!(only(Client::Goose, &env).path, home.join("goose-root").join("config").join("config.yaml"));
    assert_eq!(only(Client::Hermes, &env).path, home.join("h").join("config.yaml"));

    // LM Studio's folder, moved, with a pointer file saying where.
    let moved = home.join("Models").join("lmstudio");
    std::fs::create_dir_all(&moved).unwrap();
    write(&home.join(".lmstudio-home-pointer"), &format!("{}\n", moved.display()));
    assert_eq!(only(Client::LmStudio, &env).path, moved.join("mcp.json"));
    assert!(Client::LmStudio.installed(&env));

    // A .jsonc file is used when it is the only one.
    write(&home.join(".config").join("opencode").join("opencode.jsonc"), "{}");
    assert_eq!(only(Client::OpenCode, &env).path, home.join(".config").join("opencode").join("opencode.jsonc"));
    write(&home.join(".config").join("opencode").join("opencode.json"), "{}");
    assert_eq!(only(Client::OpenCode, &env).path, home.join(".config").join("opencode").join("opencode.json"));

    // Claude Code still reads an older, non-empty .config.json in the profile's folder first.
    write(&home.join(".claude").join(".config.json"), "{\"numStartups\": 1}");
    assert_eq!(Client::ClaudeCode.targets(&env)[0].path, home.join(".claude").join(".config.json"));
    write(&home.join(".claude").join(".config.json"), "");
    assert_eq!(Client::ClaudeCode.targets(&env)[0].path, home.join(".claude.json"));
}

/// A server registered with a narrower view: the flags are written in every format, read
/// back as the same view, and `--all` with a view named makes every entry match it.
#[test]
fn a_narrower_view_is_written_read_back_and_applied_to_all_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path());
    let home = dir.path();
    for folder in [".cursor", ".codex", ".hermes", ".gemini"] {
        std::fs::create_dir_all(home.join(folder)).unwrap();
    }
    write(&home.join(".hermes").join("config.yaml"), "model:\n  default: some-model\n");
    write(&env.goose_config(), "extensions: {}\n");
    let view = Scope::new(&["note".to_string(), "meeting".to_string()], &["keyboard".to_string()]).unwrap();
    let mut args = vec!["mcp".to_string(), "--read-only".to_string()];
    args.extend(view.to_args());
    assert_eq!(args, ["mcp", "--read-only", "--kinds", "note,meeting", "--exclude-sources", "keyboard"]);
    let narrow = Launch { args, ..launch() };
    assert_eq!((narrow.read_only(), narrow.scope()), (true, Some(view.clone())));
    assert_eq!(launch().scope(), Some(Scope::default()));

    // JSON, TOML and both YAML shapes hold the flags and give them back.
    let mut state = State::default();
    for client in [Client::Cursor, Client::Codex, Client::Hermes, Client::Goose] {
        let target = only(client, &env);
        assert_eq!(register(&target, &narrow, &env, &mut state, Mode::Explicit), Outcome::Registered, "{client:?}");
        assert_eq!(read_entry(&target, &env), Found::Present(narrow.clone()), "{client:?}");
    }
    let rows = survey(&env, &state, &launch());
    let standing = |client: Client| rows.iter().find(|r| r.target.client == client).unwrap().standing.clone();
    assert_eq!(standing(Client::Codex), Standing::Registered { read_only: true, scope: Some(view.clone()) });

    // Plain --all leaves a narrowed entry as it is: it is that way on purpose.
    let gemini = only(Client::Gemini, &env);
    let cursor = only(Client::Cursor, &env);
    assert_eq!(register_many(&[Client::Cursor, Client::Gemini], &launch(), Mode::All, &env, &mut state, true), 0);
    assert_eq!(read_entry(&cursor, &env), Found::Present(narrow.clone()));
    assert_eq!(read_entry(&gemini, &env), Found::Present(launch()));

    // --all with a view named: every entry becomes exactly that, the ones already there too.
    let no_keyboard = Launch { args: vec!["mcp".into(), "--exclude-sources".into(), "keyboard".into()], ..launch() };
    assert_eq!(register_with(&cursor, &no_keyboard, &env, &mut state, Mode::All, true), Outcome::Updated);
    assert_eq!(register_with(&gemini, &no_keyboard, &env, &mut state, Mode::All, true), Outcome::Updated);
    assert_eq!(register_with(&gemini, &no_keyboard, &env, &mut state, Mode::All, true), Outcome::Already);
    assert_eq!(read_entry(&cursor, &env), Found::Present(no_keyboard.clone()));
    // Still never where the person took zone out, and never by the daemon.
    unregister(&gemini, &env, &mut state, true);
    assert_eq!(register_with(&gemini, &no_keyboard, &env, &mut state, Mode::All, true), Outcome::OptedOut);
    assert_eq!(register_with(&cursor, &launch(), &env, &mut state, Mode::Automatic, true), Outcome::Already);
    assert_eq!(read_entry(&cursor, &env), Found::Present(no_keyboard));

    // Arguments someone wrote by hand that zone would refuse to start with.
    let odd = Launch { args: vec!["mcp".into(), "--kinds".into(), "songs".into()], ..launch() };
    register(&cursor, &odd, &env, &mut state, Mode::Explicit);
    let rows = survey(&env, &state, &launch());
    assert_eq!(rows.iter().find(|r| r.target.client == Client::Cursor).unwrap().standing, Standing::Registered { read_only: false, scope: None });
}

/// What an install keeps and what it changes, for an entry that is already there.
#[test]
fn an_install_keeps_what_it_was_not_asked_to_change() {
    let base = launch();
    let notes = Scope::new(&["note".to_string()], &[]).unwrap();
    let narrow = Found::Present(base.with_view(true, &notes));
    let args = |wish: Wish, existing: &Found| wish.launch_for(&base, existing).unwrap().args;
    // Nothing asked: a new place gets everything, a narrowed one stays narrowed and read-only.
    assert_eq!(args(Wish::default(), &Found::Absent), ["mcp"]);
    assert_eq!(args(Wish::default(), &narrow), ["mcp", "--read-only", "--kinds", "note"]);
    // Each part changes only when named.
    assert_eq!(args(Wish { read_write: true, ..Wish::default() }, &narrow), ["mcp", "--kinds", "note"]);
    assert_eq!(args(Wish { view_named: true, ..Wish::default() }, &narrow), ["mcp", "--read-only"]);
    let no_keyboard = Scope::new(&[], &["keyboard".to_string()]).unwrap();
    assert_eq!(args(Wish { scope: no_keyboard.clone(), view_named: true, read_write: true, ..Wish::default() }, &narrow), ["mcp", "--exclude-sources", "keyboard"]);
    assert_eq!(args(Wish { read_only: true, ..Wish::default() }, &Found::Present(base.clone())), ["mcp", "--read-only"]);
    // An entry that starts another copy of zone keeps its view when it is pointed here.
    let other = Found::Present(Launch { command: "/old/zone".into(), ..base.with_view(true, &notes) });
    assert_eq!(args(Wish::default(), &other), ["mcp", "--read-only", "--kinds", "note"]);
    assert_eq!(Wish::default().launch_for(&base, &other).unwrap().command, base.command);
    // Something else that happens to be registered under zone's name is not a view to keep.
    let stranger = Found::Present(Launch { command: "/usr/bin/other".into(), args: vec!["--stdio".into()], env: Vec::new() });
    assert_eq!(args(Wish::default(), &stranger), ["mcp"]);
    // Arguments that cannot be read are not guessed at, unless a view is named.
    let odd = Found::Present(Launch { args: vec!["mcp".into(), "--kinds".into(), "songs".into()], ..base.clone() });
    assert!(Wish::default().launch_for(&base, &odd).unwrap_err().contains("will not guess"));
    assert_eq!(args(Wish { view_named: true, ..Wish::default() }, &odd), ["mcp"]);
    assert_eq!(base.with_view(true, &no_keyboard).view_words().unwrap(), "read-only, hides keyboard");
    assert_eq!(base.view_words(), None);
}

#[test]
fn snippets_to_paste_are_what_an_install_would_write() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path());
    let generic = generic_snippet(&with_home());
    let value: Value = serde_json::from_str(&generic.text).unwrap();
    assert_eq!(value, json!({ "mcpServers": { "zone": { "command": "/opt/zone/bin/zone", "args": ["mcp"], "env": { "ZONE_HOME": "/tmp/zone home" } } } }));
    assert_eq!(generic.key_path, ["mcpServers", "zone"]);
    // Each client's snippet, saved as its whole settings file, reads back as this zone.
    for client in Client::ALL {
        let snippet = client_snippet(client, &with_home(), &env);
        let target = client.targets(&env).into_iter().next().unwrap();
        assert_eq!(snippet.path.as_deref(), Some(target.path.as_path()), "{client:?}");
        write(&target.path, &snippet.text);
        assert_eq!(read_entry(&target, &env), Found::Present(with_home()), "{client:?}:\n{}", snippet.text);
        assert_eq!(snippet.key_path.last().map(String::as_str), Some(SERVER_NAME));
        let format = match target.format {
            Format::Json(..) => "json",
            Format::Toml => "toml",
            Format::Yaml(..) => "yaml",
        };
        assert_eq!(snippet.format, format, "{client:?}");
        assert_eq!(snippet.command_line.is_some(), client == Client::ClaudeCode);
    }
    // A limited, read-only view goes into the snippet.
    let narrow = with_home().with_view(true, &Scope::new(&["note".to_string()], &[]).unwrap());
    let text = generic_snippet(&narrow).text;
    assert!(text.contains("\"--read-only\"") && text.contains("\"--kinds\"") && text.contains("\"note\""), "{text}");
}

#[test]
fn status_says_what_each_place_launches() {
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path());
    let target = only(Client::Cursor, &env);
    register(&target, &with_home(), &env, &mut State::default(), Mode::Explicit);
    let rows = survey(&env, &State::default(), &with_home());
    let value = rows_json(&rows, &with_home(), &State::default());
    let cursor = value["clients"].as_array().unwrap().iter().find(|c| c["client"] == "cursor").unwrap();
    assert_eq!(cursor["launch"], json!({ "command": "/opt/zone/bin/zone", "args": ["mcp"], "env": { "ZONE_HOME": "/tmp/zone home" } }));
    let zed = value["clients"].as_array().unwrap().iter().find(|c| c["client"] == "zed").unwrap();
    assert_eq!(zed["launch"], Value::Null);
}
