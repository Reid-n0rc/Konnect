//! Client-aware installer for Konnect's bundled guidance.
//!
//! Handles explicit client-scoped install, uninstall, status, and Claude hook
//! integration without writing into another client's directories.

use crate::manifest::{AGENTS, HOOK_SKILLS, SKILLS};
use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InstallClient {
    #[default]
    Claude,
    Codex,
}

impl InstallClient {
    fn marker_name(self) -> &'static str {
        match self {
            Self::Claude => ".installed-claude",
            Self::Codex => ".installed-codex",
        }
    }
}

impl fmt::Display for InstallClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Claude => write!(f, "Claude"),
            Self::Codex => write!(f, "Codex"),
        }
    }
}

impl FromStr for InstallClient {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => bail!("unsupported client '{value}'; expected 'claude' or 'codex'"),
        }
    }
}

/// Parse `--client <claude|codex>` from a subcommand's arguments, refusing any
/// argument this build does not recognise. Claude remains the default for
/// compatibility.
///
/// Skipping unrecognised arguments is how `konnect init --help` came to run the
/// installer (#238): `--help` was not `--client`, so it was dropped on the
/// floor, the client defaulted to Claude, and a command that looks like
/// documentation rewrote `~/.claude`. A misspelled flag has the same shape —
/// it changes nothing the caller asked for and everything they did not.
pub fn client_from_args(args: &[String]) -> Result<InstallClient> {
    client_from_args_allowing(args, &[])
}

/// As [`client_from_args`], for the server invocation — which also carries
/// `--config <path>`, parsed elsewhere in `main`.
pub fn client_from_server_args(args: &[String]) -> Result<InstallClient> {
    client_from_args_allowing(args, &["--config"])
}

/// `also` names options that take a value and that this parser should step
/// over rather than reject.
fn client_from_args_allowing(args: &[String], also: &[&str]) -> Result<InstallClient> {
    let mut selected = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--client" {
            if selected.is_some() {
                bail!("--client may only be specified once");
            }
            let value = args
                .get(index + 1)
                .context("--client requires 'claude' or 'codex'")?;
            selected = Some(value.parse()?);
            index += 2;
        } else if also.contains(&arg) {
            index += 2;
        } else {
            bail!("unrecognised argument '{arg}'; run 'konnect --help' for usage");
        }
    }
    Ok(selected.unwrap_or_default())
}

pub fn run_install(client: InstallClient) -> Result<()> {
    run_install_at(client, &InstallPaths::for_current_user()?, true)
}

pub fn run_uninstall(client: InstallClient) -> Result<()> {
    run_uninstall_at(client, &InstallPaths::for_current_user()?, true)
}

pub fn print_status(client: InstallClient) -> Result<()> {
    print_status_at(client, &InstallPaths::for_current_user()?)
}

/// Print bundled guidance for Claude hook integration.
pub fn print_skill_content(name: &str) -> Result<()> {
    for hook in HOOK_SKILLS {
        if hook.name == name {
            print!("{}", hook.content);
            return Ok(());
        }
    }
    for skill in SKILLS {
        if skill.name == name {
            print!("{}", skill.content);
            return Ok(());
        }
    }
    eprintln!("Unknown skill: {}", name);
    std::process::exit(1);
}

/// Emit the structured response Claude consumes for a hook event. Stdout is
/// intentionally one JSON object and nothing else.
pub fn print_hook_output(name: &str) -> Result<()> {
    let hook = HOOK_SKILLS
        .iter()
        .find(|hook| hook.name == name)
        .with_context(|| format!("unknown hook: {name}"))?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": hook.event,
                "additionalContext": hook.content
            }
        }))?
    );
    Ok(())
}

fn hook_tool_names(board_access: konnect_core::tools::BoardAccess) -> Vec<&'static str> {
    let mut names = BTreeSet::new();
    for toolset in konnect_core::router::registry::ALL_TOOLSETS {
        if let Some(tools) = konnect_core::router::registry::tools_for(toolset.name) {
            for tool in tools {
                if tool.board_access == board_access {
                    names.insert(tool.name);
                }
            }
        }
    }
    names.into_iter().collect()
}

fn hook_matcher(hook: &crate::manifest::HookSkillManifest) -> Result<String> {
    let names = hook_tool_names(hook.board_access);
    if names.is_empty() {
        anyhow::bail!("hook '{}' has no registered tool targets", hook.name);
    }
    Ok(format!("mcp__konnect__({})", names.join("|")))
}

fn quote_command_arg(argument: &str) -> String {
    #[cfg(windows)]
    {
        format!("\"{}\"", argument.replace('"', "\\\""))
    }
    #[cfg(not(windows))]
    {
        format!("'{}'", argument.replace('\'', "'\\''"))
    }
}

fn hook_command(exe_str: &str, subcommand: &str, hook_name: &str) -> String {
    format!(
        "{} {} {}",
        quote_command_arg(exe_str),
        subcommand,
        quote_command_arg(hook_name)
    )
}

/// Exact command representation written by releases before the structured
/// hook subcommand. It was unquoted and doubled Windows backslashes before
/// JSON serialization; retain this only for surgical migration/removal.
fn legacy_hook_command(exe_str: &str, hook_name: &str) -> String {
    format!("{} skill {}", exe_str.replace('\\', "\\\\"), hook_name)
}

/// Double-click behavior remains Claude-focused for backward compatibility.
pub fn run_double_click_install() -> Result<()> {
    println!("===========================================");
    println!("  Konnect v{}", env!("CARGO_PKG_VERSION"));
    println!("  First-time Setup");
    println!("===========================================\n");
    run_install(InstallClient::Claude)?;

    let exe = std::env::current_exe()?;
    let exe_str = exe.to_string_lossy().replace('\\', "\\\\");
    println!("\n-------------------------------------------");
    println!("Add this to your Claude MCP config:");
    println!("-------------------------------------------\n");
    println!(r#"  "konnect": {{"#);
    println!(r#"    "command": "{}","#, exe_str);
    println!(r#"    "env": {{ "RUST_LOG": "info" }}"#);
    println!(r#"  }}"#);
    println!("\nConfig locations:");
    println!("  Claude Desktop: %APPDATA%\\Claude\\claude_desktop_config.json");
    println!("  Claude Code:    .mcp.json in your project root");
    println!("\nAfter editing the config, restart Claude.\n");
    println!("Press Enter to close...");
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
    Ok(())
}

#[derive(Debug)]
struct InstallPaths {
    home: PathBuf,
}

impl InstallPaths {
    fn for_current_user() -> Result<Self> {
        Ok(Self {
            home: dirs::home_dir().context("could not locate home directory")?,
        })
    }

    fn data_dir(&self) -> PathBuf {
        self.home.join(".konnect")
    }

    fn skills_dir(&self, client: InstallClient) -> PathBuf {
        match client {
            InstallClient::Claude => self.home.join(".claude").join("skills"),
            InstallClient::Codex => self.home.join(".agents").join("skills"),
        }
    }

    fn claude_agents_dir(&self) -> PathBuf {
        self.home.join(".claude").join("agents")
    }

    fn claude_settings_path(&self) -> PathBuf {
        self.home.join(".claude").join("settings.json")
    }

    fn marker(&self, client: InstallClient) -> PathBuf {
        self.data_dir().join(client.marker_name())
    }

    fn legacy_marker(&self) -> PathBuf {
        self.data_dir().join(".installed")
    }
}

fn run_install_at(client: InstallClient, paths: &InstallPaths, verbose: bool) -> Result<()> {
    if verbose {
        match client {
            InstallClient::Claude => {
                println!("Installing Konnect skills, agents, and hooks for Claude...\n")
            }
            InstallClient::Codex => println!("Installing Konnect skills for Codex...\n"),
        }
    }

    let skill_count = install_skills(client, paths, verbose)?;
    let mut agent_count = 0;
    let mut hook_count = 0;
    if client == InstallClient::Claude {
        let agents_dir = paths.claude_agents_dir();
        fs::create_dir_all(&agents_dir)?;
        for agent in AGENTS {
            fs::write(agents_dir.join(agent.filename), agent.content)?;
            agent_count += 1;
            if verbose {
                println!("  [+] Agent: {}", agent.filename);
            }
        }

        let exe = std::env::current_exe()?;
        let settings_path = paths.claude_settings_path();
        let exe_str = exe.to_string_lossy();
        hook_count = if verbose {
            patch_claude_settings(&settings_path, &exe_str)?
        } else {
            patch_claude_settings(&settings_path, &exe_str).unwrap_or_default()
        };
        if verbose {
            if hook_count > 0 {
                println!("  [+] Hooks: {hook_count} entries patched into settings.json");
            } else {
                println!("  [=] Hooks: already installed (no changes)");
            }
        }
    }

    if verbose {
        if let Some(kicad_path) = detect_kicad() {
            println!("\n  [+] Found KiCAD at: {}", kicad_path.display());
        } else {
            println!("\n  [-] KiCAD not found in standard locations");
            println!("      Set kicad_cli path in your config file manually");
        }
    }

    fs::create_dir_all(paths.data_dir())?;
    fs::write(paths.marker(client), env!("CARGO_PKG_VERSION"))?;

    if verbose {
        match client {
            InstallClient::Claude => println!(
                "\nDone: {skill_count} skills, {agent_count} agents, {hook_count} hooks installed for Claude."
            ),
            InstallClient::Codex => {
                println!("\nDone: {skill_count} skills installed for Codex.")
            }
        }
    } else {
        eprintln!(
            "[konnect] Silent {client} install complete: {skill_count} skills, {agent_count} agents"
        );
    }
    Ok(())
}

fn install_skills(client: InstallClient, paths: &InstallPaths, verbose: bool) -> Result<usize> {
    let skills_dir = paths.skills_dir(client);
    for skill in SKILLS {
        let dest = skills_dir.join(skill.name);
        fs::create_dir_all(&dest)?;
        fs::write(dest.join("SKILL.md"), skill.content)?;
        if !skill.references.is_empty() {
            let refs_dir = dest.join("references");
            fs::create_dir_all(&refs_dir)?;
            for (filename, content) in skill.references {
                fs::write(refs_dir.join(filename), content)?;
            }
        }
        if verbose {
            println!("  [+] Skill: {}", skill.name);
        }
    }
    Ok(SKILLS.len())
}

fn run_uninstall_at(client: InstallClient, paths: &InstallPaths, verbose: bool) -> Result<()> {
    if verbose {
        println!("Uninstalling Konnect guidance for {client}...\n");
    }

    let skills_dir = paths.skills_dir(client);
    for skill in SKILLS {
        let dest = skills_dir.join(skill.name);
        if dest.exists() {
            fs::remove_dir_all(&dest)?;
            if verbose {
                println!("  [-] Removed skill: {}", skill.name);
            }
        }
    }

    if client == InstallClient::Claude {
        let agents_dir = paths.claude_agents_dir();
        for agent in AGENTS {
            let dest = agents_dir.join(agent.filename);
            if dest.exists() {
                fs::remove_file(&dest)?;
                if verbose {
                    println!("  [-] Removed agent: {}", agent.filename);
                }
            }
        }
        let exe = std::env::current_exe()?;
        remove_hooks_from_settings(
            &paths.claude_settings_path(),
            exe.to_string_lossy().as_ref(),
        )?;
        if verbose {
            println!("  [-] Removed hook entries from settings.json");
        }
        remove_if_present(&paths.legacy_marker())?;
    }

    remove_if_present(&paths.marker(client))?;
    if verbose {
        println!("\nDone.");
    }
    Ok(())
}

fn print_status_at(client: InstallClient, paths: &InstallPaths) -> Result<()> {
    println!(
        "Konnect v{} — {client} Install Status\n",
        env!("CARGO_PKG_VERSION")
    );
    let skills_dir = paths.skills_dir(client);
    println!("Skills ({}):", display_home_path(&skills_dir, &paths.home));
    for skill in SKILLS {
        let marker = if skills_dir.join(skill.name).join("SKILL.md").exists() {
            "+"
        } else {
            "-"
        };
        println!("  [{marker}] {}", skill.name);
    }

    if client == InstallClient::Claude {
        let agents_dir = paths.claude_agents_dir();
        println!("\nAgents (~/.claude/agents/):");
        for agent in AGENTS {
            let marker = if agents_dir.join(agent.filename).exists() {
                "+"
            } else {
                "-"
            };
            println!("  [{marker}] {}", agent.filename);
        }
        println!("\nHooks (~/.claude/settings.json):");
        let raw = fs::read_to_string(paths.claude_settings_path()).unwrap_or_default();
        for hook in HOOK_SKILLS {
            let marker = if raw.contains(hook.name) { "+" } else { "-" };
            println!("  [{marker}] {} ({})", hook.name, hook.event);
        }
    }

    println!("\nKiCAD:");
    if let Some(path) = detect_kicad() {
        println!("  [+] Found: {}", path.display());
    } else {
        println!("  [-] Not found in standard locations");
    }

    if let Some(marker) = install_marker(client, paths) {
        let version = fs::read_to_string(marker).unwrap_or_default();
        println!("\nInstall marker: v{}", version.trim());
    } else {
        println!("\nInstall marker: not present (never installed)");
    }
    Ok(())
}

fn install_marker(client: InstallClient, paths: &InstallPaths) -> Option<PathBuf> {
    let marker = paths.marker(client);
    if marker.exists() {
        return Some(marker);
    }
    if client == InstallClient::Claude && paths.legacy_marker().exists() {
        return Some(paths.legacy_marker());
    }
    None
}

fn remove_if_present(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn display_home_path(path: &Path, home: &Path) -> String {
    path.strip_prefix(home)
        .map(|relative| format!("~/{}", relative.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

fn patch_claude_settings(path: &Path, exe_str: &str) -> Result<usize> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = if path.exists() {
        fs::read_to_string(path)?
    } else {
        "{}".to_string()
    };
    let mut settings: serde_json::Value = serde_json::from_str(&raw)?;
    let hooks_obj = settings
        .as_object_mut()
        .context("Claude settings root is not an object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("hooks field is not an object")?;

    let mut added = 0;
    for hook in HOOK_SKILLS {
        let command = hook_command(exe_str, "hook", hook.name);
        let legacy_command = legacy_hook_command(exe_str, hook.name);
        let event_arr = hooks_obj
            .entry(hook.event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .context("hook event field is not an array")?;
        // Migrate the exact handler installed by the old plain-stdout form.
        // Do not use substring matching: user-authored neighboring handlers
        // and unrelated commands containing "konnect" are not ours.
        remove_exact_commands(event_arr, &[legacy_command.as_str()]);
        let already_exists = event_arr.iter().any(|entry| {
            entry
                .get("hooks")
                .and_then(|hooks| hooks.as_array())
                .is_some_and(|hooks| {
                    hooks.iter().any(|hook_entry| {
                        hook_entry
                            .get("command")
                            .and_then(|command| command.as_str())
                            .is_some_and(|candidate| candidate == command)
                    })
                })
        });
        if !already_exists {
            event_arr.push(serde_json::json!({
                "matcher": hook_matcher(hook)?,
                "hooks": [{
                    "type": "command",
                    "command": command
                }]
            }));
            added += 1;
        }
    }
    fs::write(path, serde_json::to_string_pretty(&settings)?)?;
    Ok(added)
}

fn remove_exact_commands(event_arr: &mut Vec<serde_json::Value>, commands: &[&str]) {
    for entry in event_arr.iter_mut() {
        if let Some(handlers) = entry
            .get_mut("hooks")
            .and_then(|hooks| hooks.as_array_mut())
        {
            handlers.retain(|handler| {
                !handler
                    .get("command")
                    .and_then(|command| command.as_str())
                    .is_some_and(|command| commands.contains(&command))
            });
        }
    }
    event_arr.retain(|entry| {
        entry
            .get("hooks")
            .and_then(|hooks| hooks.as_array())
            .is_none_or(|handlers| !handlers.is_empty())
    });
}

fn remove_hooks_from_settings(path: &Path, exe_str: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let raw = fs::read_to_string(path)?;
    let mut settings: serde_json::Value = serde_json::from_str(&raw)?;
    if let Some(hooks_obj) = settings
        .get_mut("hooks")
        .and_then(|hooks| hooks.as_object_mut())
    {
        for hook in HOOK_SKILLS {
            if let Some(event_arr) = hooks_obj
                .get_mut(hook.event)
                .and_then(|event| event.as_array_mut())
            {
                let current = hook_command(exe_str, "hook", hook.name);
                let legacy = legacy_hook_command(exe_str, hook.name);
                remove_exact_commands(event_arr, &[current.as_str(), legacy.as_str()]);
            }
        }
    }
    fs::write(path, serde_json::to_string_pretty(&settings)?)?;
    Ok(())
}

/// Auto-detect a KiCad installation.
pub fn detect_kicad() -> Option<PathBuf> {
    konnect_core::kicad_install::find_cli("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_paths(temp: &TempDir) -> InstallPaths {
        InstallPaths {
            home: temp.path().to_path_buf(),
        }
    }

    #[test]
    fn client_argument_defaults_to_claude() {
        assert_eq!(client_from_args(&[]).unwrap(), InstallClient::Claude);
    }

    /// An argument this build does not understand stops the command instead of
    /// being skipped on the way to a default.
    ///
    /// The skipping is what made `konnect init --help` install (#238), and the
    /// same hole means a typo — `--cleint codex` — silently installs for
    /// Claude, which is the surprising outcome for someone mid-way through
    /// setting up Codex.
    #[test]
    fn an_unrecognised_argument_stops_the_command() {
        for argv in [
            vec!["--help".to_string()],
            vec!["-h".to_string()],
            vec!["--cleint".to_string(), "codex".to_string()],
            vec![
                "--client".to_string(),
                "codex".to_string(),
                "extra".to_string(),
            ],
        ] {
            let error = client_from_args(&argv)
                .expect_err(&format!("{argv:?} must not resolve to a client"));
            let message = format!("{error:#}");
            assert!(
                message.contains("unrecognised argument") || message.contains("unsupported client"),
                "{argv:?}: {message}"
            );
        }
    }

    /// The server invocation carries `--config <path>`, which the bundled
    /// `examples/*.json` tell users to write, so it must still parse.
    #[test]
    fn server_arguments_accept_config_and_still_reject_the_unknown() {
        let with_config = vec![
            "--config".to_string(),
            "C:/konnect.json".to_string(),
            "--client".to_string(),
            "codex".to_string(),
        ];
        assert_eq!(
            client_from_server_args(&with_config).unwrap(),
            InstallClient::Codex
        );
        assert_eq!(
            client_from_server_args(&["--config".to_string(), "C:/k.json".to_string()]).unwrap(),
            InstallClient::Claude
        );
        assert!(client_from_server_args(&["--nope".to_string()]).is_err());
    }

    #[test]
    fn client_argument_selects_codex() {
        let args = vec!["--client".into(), "codex".into()];
        assert_eq!(client_from_args(&args).unwrap(), InstallClient::Codex);
    }

    #[test]
    fn client_argument_rejects_bad_values() {
        assert!(client_from_args(&["--client".into()]).is_err());
        assert!(client_from_args(&["--client".into(), "other".into()]).is_err());
        assert!(client_from_args(&[
            "--client".into(),
            "codex".into(),
            "--client".into(),
            "claude".into(),
        ])
        .is_err());
    }

    #[test]
    fn codex_install_writes_skills_without_touching_claude() {
        let temp = TempDir::new().unwrap();
        let paths = test_paths(&temp);
        run_install_at(InstallClient::Codex, &paths, false).unwrap();

        for skill in SKILLS {
            let skill_dir = paths.skills_dir(InstallClient::Codex).join(skill.name);
            assert_eq!(
                fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
                skill.content
            );
            for (filename, content) in skill.references {
                assert_eq!(
                    fs::read_to_string(skill_dir.join("references").join(filename)).unwrap(),
                    *content
                );
            }
        }
        assert!(!temp.path().join(".claude").exists());
        assert!(paths.marker(InstallClient::Codex).exists());
        assert!(!paths.marker(InstallClient::Claude).exists());
    }

    #[test]
    fn claude_install_is_idempotent_and_preserves_settings() {
        let temp = TempDir::new().unwrap();
        let paths = test_paths(&temp);
        let settings_path = paths.claude_settings_path();
        fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        fs::write(&settings_path, r#"{"theme":"dark"}"#).unwrap();

        run_install_at(InstallClient::Claude, &paths, false).unwrap();
        run_install_at(InstallClient::Claude, &paths, false).unwrap();

        for skill in SKILLS {
            assert!(paths
                .skills_dir(InstallClient::Claude)
                .join(skill.name)
                .join("SKILL.md")
                .exists());
        }
        for agent in AGENTS {
            assert!(paths.claude_agents_dir().join(agent.filename).exists());
        }
        let settings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(settings_path).unwrap()).unwrap();
        assert_eq!(settings["theme"], "dark");
        let entries = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), HOOK_SKILLS.len());
        for hook in HOOK_SKILLS {
            let expected_command = hook_command(
                std::env::current_exe().unwrap().to_string_lossy().as_ref(),
                "hook",
                hook.name,
            );
            assert!(entries.iter().any(|entry| {
                entry["matcher"] == hook_matcher(hook).unwrap()
                    && entry["hooks"][0]["command"] == expected_command
            }));
        }
    }

    #[test]
    fn claude_installs_the_canonical_reliability_contract_offline() {
        let temp = TempDir::new().unwrap();
        let paths = test_paths(&temp);
        run_install_at(InstallClient::Claude, &paths, false).unwrap();
        let skill_dir = paths.skills_dir(InstallClient::Claude).join("konnect");
        let installed = fs::read(skill_dir.join("references/reliability-contract.md")).unwrap();
        assert_eq!(
            installed,
            include_bytes!("../../../docs/RELIABILITY_CONTRACT.md")
        );
        let skill = fs::read_to_string(skill_dir.join("SKILL.md")).unwrap();
        assert!(skill.contains("](references/reliability-contract.md)"));
        for filename in [
            "kicad-schematic-build-agent.md",
            "kicad-design-review-agent.md",
        ] {
            let agent = fs::read_to_string(paths.claude_agents_dir().join(filename)).unwrap();
            assert!(
                agent.contains("references/reliability-contract.md"),
                "{filename}"
            );
        }
    }

    #[test]
    fn hook_matchers_are_derived_from_registered_board_contracts() {
        let registered = konnect_core::router::registry::ALL_TOOLSETS
            .iter()
            .flat_map(|toolset| {
                konnect_core::router::registry::tools_for(toolset.name).unwrap_or_default()
            })
            .map(|tool| (tool.name, tool.board_access))
            .collect::<std::collections::HashMap<_, _>>();

        for hook in HOOK_SKILLS {
            let matcher = hook_matcher(hook).unwrap();
            let targets = matcher
                .strip_prefix("mcp__konnect__(")
                .and_then(|value| value.strip_suffix(')'))
                .unwrap()
                .split('|')
                .collect::<Vec<_>>();
            assert!(!targets.is_empty());
            for target in targets {
                assert_eq!(registered.get(target), Some(&hook.board_access), "{target}");
            }
        }
        assert!(!HOOK_SKILLS
            .iter()
            .any(|hook| hook_matcher(hook).unwrap().contains("refill_zones")));
        assert_eq!(
            registered.get("route_trace"),
            Some(&konnect_core::tools::BoardAccess::LiveOnly)
        );
        for tool_name in ["plan_specctra_ses_import", "apply_specctra_ses"] {
            assert_eq!(
                registered.get(tool_name),
                Some(&konnect_core::tools::BoardAccess::LiveOnly),
                "{tool_name} must remain in the live-only Claude hook class"
            );
        }
        assert_eq!(
            registered.get("place_component"),
            Some(&konnect_core::tools::BoardAccess::LivePreferredWithFallback)
        );
        // #604: flip_component now prefers KiCad 10.0.6's native live-IPC
        // FlipItems, with the closed-board file edit as its guarded fallback
        // — the pre-pcb-fallback hook's own guidance, not pre-pcb-closed's.
        assert_eq!(
            registered.get("flip_component"),
            Some(&konnect_core::tools::BoardAccess::LivePreferredWithFallback)
        );
        assert_eq!(
            registered.get("plan_bga_fanout"),
            Some(&konnect_core::tools::BoardAccess::ApplyModeDependent)
        );
    }

    #[test]
    fn spaced_windows_executable_survives_settings_json_roundtrip() {
        let temp = TempDir::new().unwrap();
        let settings_path = temp.path().join("settings.json");
        let exe = r"C:\Program Files\Konnect Tools\konnect.exe";

        patch_claude_settings(&settings_path, exe).unwrap();
        let settings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(settings_path).unwrap()).unwrap();
        let commands = settings["hooks"]["PreToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|entry| entry["hooks"].as_array().unwrap())
            .map(|handler| handler["command"].as_str().unwrap())
            .collect::<Vec<_>>();

        for hook in HOOK_SKILLS {
            assert!(commands.contains(&hook_command(exe, "hook", hook.name).as_str()));
        }
        #[cfg(windows)]
        assert!(commands.iter().all(|command| command.starts_with('"')));
    }

    #[test]
    fn install_migrates_the_exact_legacy_plain_stdout_handler() {
        let temp = TempDir::new().unwrap();
        let settings_path = temp.path().join("settings.json");
        let exe = r"C:\Program Files\Konnect\konnect.exe";
        let hook = &HOOK_SKILLS[0];
        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "hooks": {
                    hook.event: [{
                        "matcher": "old-static-matcher",
                        "hooks": [{
                            "type": "command",
                            "command": legacy_hook_command(exe, hook.name)
                        }, {
                            "type": "command",
                            "command": "user-neighbor"
                        }]
                    }]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        patch_claude_settings(&settings_path, exe).unwrap();
        let settings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(settings_path).unwrap()).unwrap();
        let commands = settings["hooks"][hook.event]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|entry| entry["hooks"].as_array().unwrap())
            .map(|handler| handler["command"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(!commands.contains(&legacy_hook_command(exe, hook.name).as_str()));
        assert!(commands.contains(&hook_command(exe, "hook", hook.name).as_str()));
        assert!(commands.contains(&"user-neighbor"));
    }

    #[test]
    fn legacy_marker_applies_to_claude_only() {
        let temp = TempDir::new().unwrap();
        let paths = test_paths(&temp);
        fs::create_dir_all(paths.data_dir()).unwrap();
        fs::write(paths.legacy_marker(), "0.4.0").unwrap();
        assert!(install_marker(InstallClient::Claude, &paths).is_some());
        assert!(install_marker(InstallClient::Codex, &paths).is_none());
    }

    #[test]
    fn codex_uninstall_is_scoped() {
        let temp = TempDir::new().unwrap();
        let paths = test_paths(&temp);
        run_install_at(InstallClient::Claude, &paths, false).unwrap();
        run_install_at(InstallClient::Codex, &paths, false).unwrap();
        let unrelated = paths.skills_dir(InstallClient::Codex).join("my-skill");
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(unrelated.join("SKILL.md"), "keep me").unwrap();

        run_uninstall_at(InstallClient::Codex, &paths, false).unwrap();
        assert!(unrelated.join("SKILL.md").exists());
        assert!(paths.marker(InstallClient::Claude).exists());
        assert!(!paths.marker(InstallClient::Codex).exists());
        assert!(paths.claude_settings_path().exists());
    }

    #[test]
    fn claude_uninstall_preserves_other_hooks() {
        let temp = TempDir::new().unwrap();
        let paths = test_paths(&temp);
        run_install_at(InstallClient::Claude, &paths, false).unwrap();
        fs::write(paths.legacy_marker(), "0.3.0").unwrap();

        let settings_path = paths.claude_settings_path();
        let mut settings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        settings["hooks"][HOOK_SKILLS[0].event]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "matcher": "Write",
                "hooks": [{"type": "command", "command": "other-tool"}]
            }));
        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        run_uninstall_at(InstallClient::Claude, &paths, false).unwrap();
        assert!(!paths.marker(InstallClient::Claude).exists());
        assert!(!paths.legacy_marker().exists());
        let remaining: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(settings_path).unwrap()).unwrap();
        let entries = remaining["hooks"][HOOK_SKILLS[0].event].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["hooks"][0]["command"], "other-tool");
    }

    #[test]
    fn exact_uninstall_preserves_mixed_and_similarly_named_handlers() {
        let temp = TempDir::new().unwrap();
        let settings_path = temp.path().join("settings.json");
        let exe = r"C:\Program Files\Konnect\konnect.exe";
        patch_claude_settings(&settings_path, exe).unwrap();

        let mut settings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        let entries = settings["hooks"]["PreToolUse"].as_array_mut().unwrap();
        entries[0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "type": "command",
                "command": "user-board-check"
            }));
        entries.push(serde_json::json!({
            "matcher": "Write",
            "hooks": [{
                "type": "command",
                "command": "C:/tools/konnect-helper.exe audit"
            }]
        }));
        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        remove_hooks_from_settings(&settings_path, exe).unwrap();
        let remaining = fs::read_to_string(settings_path).unwrap();
        assert!(remaining.contains("user-board-check"));
        assert!(remaining.contains("konnect-helper.exe audit"));
        for hook in HOOK_SKILLS {
            assert!(!remaining.contains(&hook_command(exe, "hook", hook.name)));
        }
    }
}
