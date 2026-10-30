//! `decanter ps settings`: a game's own settings, or the defaults new games copy, from the command
//! line, through the same operations as a game's page and the defaults page
//! ([`decanter_ps::bbport::per_game`], [`decanter_ps::bbport::transfer`]), for night scripts and
//! Windows tests. Nothing that replaces a game's settings happens without `--yes`; without it the
//! command says what it would do. A game's own switches set or imported here are checked against
//! the list the port gives for that game when the port is built here; the port lists no other ini
//! keys, so those keep the shape checks.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use decanter_core::Db;
use decanter_ps::bbport::per_game::{self, Category, DEFAULTS, IniCheck, PerGameError, Refusal, Target};
use decanter_ps::bbport::{Bbport, Store, effects, transfer};
use decanter_ps::paths::{self, Root};
use decanter_ps::port::Port;
use decanter_ps::title_id::TitleId;

use crate::i18n::Text;
use crate::state::per_game_error;

/// A file that could not be read or written.
const FAILED: i32 = 1;
/// A request refused before anything was written: a key, a value, a file or a title not taken.
const REFUSED: i32 = 2;

#[derive(Subcommand)]
pub enum SettingsCmd {
    /// Without --title, the games with settings of their own; with it, every setting it sets, one
    /// per line: category, file (launch or ini), key and value.
    List {
        #[arg(long, value_parser = target)]
        title: Option<String>,
    },
    /// One setting's value; nothing is printed when it is unset and the default applies.
    Get {
        #[arg(long, value_parser = target)]
        title: String,
        #[arg(long)]
        key: String,
    },
    /// Changes one setting, checked as an import checks it; an empty value removes it, so the default
    /// applies again. A switch takes on or off.
    Set {
        #[arg(long, value_parser = target)]
        title: String,
        #[arg(long)]
        key: String,
        #[arg(long, allow_hyphen_values = true)]
        value: String,
    },
    /// Copies another game's settings over this one's (--title), or over every other game of the
    /// port in the library (--to all): all of them, or each --category given (graphics, keyboard,
    /// gamepad, mods, online, extras). --to all never copies the mods and patches folders, and the
    /// online settings only reach games that play online. Needs --yes.
    Copy {
        #[arg(long, value_parser = target)]
        from: String,
        #[arg(long, value_parser = target, required_unless_present = "to", conflicts_with = "to")]
        title: Option<String>,
        #[arg(long, value_parser = ["all"])]
        to: Option<String>,
        #[arg(long = "category", value_name = "CATEGORY", value_parser = category)]
        categories: Vec<Category>,
        #[arg(long)]
        yes: bool,
    },
    /// Puts a game back to the defaults for new games, keeping its mods and patches. Needs --yes.
    Reset {
        #[arg(long, value_parser = title_only)]
        title: String,
        #[arg(long)]
        yes: bool,
    },
    /// Drops a game's settings and port ini, after a backup, so it takes the defaults for new games
    /// the next time it is opened or played; its mods and patches stay. Needs --yes.
    Forget {
        #[arg(long, value_parser = title_only)]
        title: String,
        #[arg(long)]
        yes: bool,
    },
    /// Writes a game's settings to one JSON file; an existing file is never replaced.
    Export {
        #[arg(long, value_parser = target)]
        title: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Replaces a game's settings with those of an exported file, checked whole first. A file of
    /// another game is refused unless --any-title. Needs --yes.
    Import {
        #[arg(long, value_parser = target)]
        title: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        any_title: bool,
        #[arg(long)]
        yes: bool,
    },
}

/// `defaults` or a title id, before it names a folder.
fn target(value: &str) -> Result<String, String> {
    if value == DEFAULTS || value.parse::<TitleId>().is_ok() {
        Ok(value.to_owned())
    } else {
        Err(format!("{value} is neither a title id like CUSA03173 nor {DEFAULTS}"))
    }
}

fn title_only(value: &str) -> Result<String, String> {
    value
        .parse::<TitleId>()
        .map(|_| value.to_owned())
        .map_err(|_| format!("{value} isn't a title id like CUSA03173"))
}

fn category(value: &str) -> Result<Category, String> {
    Category::parse(value).ok_or_else(|| {
        let known: Vec<&str> = Category::ALL.iter().map(|category| category.id()).collect();
        format!("{value} isn't one of {}", known.join(", "))
    })
}

/// What `execute` takes from what can't be read on its blocking thread: the port's games in the
/// library, for `copy --to all`, and the switches the port lists for the title set or imported.
#[derive(Default)]
struct Found {
    games: Vec<(String, PathBuf)>,
    listed: Option<Vec<String>>,
}

/// The process's exit code: 0 done or shown, [`FAILED`], or [`REFUSED`].
pub async fn run(db: &Db, cmd: SettingsCmd) -> i32 {
    let (root, library_roots) = match library_root(db) {
        Ok(found) => found,
        Err(why) => {
            eprintln!("error: {why}");
            return REFUSED;
        }
    };
    let found = Found {
        games: if matches!(cmd, SettingsCmd::Copy { to: Some(_), .. }) {
            port_games(&root, &library_roots).await
        } else {
            Vec::new()
        },
        listed: listed(&root, &cmd).await,
    };
    let done = tokio::task::spawn_blocking(move || execute(&root, cmd, &found)).await;
    match done {
        Ok(Ok(())) => 0,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "decanter ps settings refused or failed");
            eprintln!("error: {}", per_game_error(&err));
            if matches!(err, PerGameError::Io(_)) {
                FAILED
            } else {
                REFUSED
            }
        }
        Err(err) => {
            tracing::warn!(%err, "decanter ps settings did not finish");
            FAILED
        }
    }
}

/// The bbport games the library lists, by title id and folder, as the PS section's scan has them.
async fn port_games(root: &Root, library_roots: &[PathBuf]) -> Vec<(String, PathBuf)> {
    super::ps_run::scan(root, library_roots)
        .await
        .into_iter()
        .filter(|entry| entry.port == Some(Port::Bbport))
        .map(|entry| (entry.game.title_id, entry.game.dir))
        .collect()
}

/// The switches the port lists for the title `cmd` sets one of or imports into, when it has any to
/// check; `None` without a list: the defaults, a port not built here, or one that can't list them.
async fn listed(root: &Root, cmd: &SettingsCmd) -> Option<Vec<String>> {
    let (title, has_switches) = match cmd {
        SettingsCmd::Set { title, key, value } => (title, !value.is_empty() && per_game::is_effect_key(key)),
        SettingsCmd::Import { title, file, .. } => {
            let file = file.clone();
            let keys = tokio::task::spawn_blocking(move || transfer::ini_keys(&file)).await;
            let has_switches = matches!(keys, Ok(Ok(keys)) if keys.iter().any(|key| per_game::is_effect_key(key)));
            (title, has_switches)
        }
        SettingsCmd::List { .. }
        | SettingsCmd::Get { .. }
        | SettingsCmd::Copy { .. }
        | SettingsCmd::Reset { .. }
        | SettingsCmd::Forget { .. }
        | SettingsCmd::Export { .. } => return None,
    };
    if !has_switches || title == DEFAULTS {
        return None;
    }
    let toolchain = decanter_ps::os::linux::toolchain::ready(root, Port::Bbport).await?;
    let sections = effects::list(&Bbport::new(root.clone()), &toolchain, title).await?;
    Some(
        sections
            .into_iter()
            .flat_map(|section| section.effects)
            .map(|effect| effect.key)
            .collect(),
    )
}

fn library_root(db: &Db) -> Result<(Root, Vec<PathBuf>), Text> {
    let profile = match db.active_profile() {
        Ok(Some(profile)) => profile,
        Ok(None) => return Err(Text::new("ps.report.errors.no_profile")),
        Err(err) => return Err(Text::new("ps.run.errors.profile").arg("error", err)),
    };
    if let Some(path) = crate::core::missing_library(&profile.id) {
        return Err(Text::new("ps.errors.library_missing").arg("path", path.display()));
    }
    let library = crate::core::library_for_profile(&profile.id)
        .map_err(|err| Text::new("ps.run.errors.library").arg("error", err))?;
    paths::use_library(library.root());
    Ok((paths::active(), library.roots()))
}

fn execute(root: &Root, cmd: SettingsCmd, found: &Found) -> Result<(), PerGameError> {
    match cmd {
        SettingsCmd::List { title: None } => {
            let titles = per_game::titles(root);
            if titles.is_empty() {
                eprintln!("{}", Text::new("ps.settings_cli.no_titles"));
            }
            for title in titles {
                println!("{title}");
            }
        }
        SettingsCmd::List { title: Some(title) } => {
            let entries = per_game::list(root, &Target::parse(root, &title)?)?;
            if entries.is_empty() {
                eprintln!("{}", Text::new("ps.settings_cli.empty").arg("title", &title));
            }
            for entry in entries {
                let file = match entry.store {
                    Store::Launcher => "launch",
                    Store::Ini => "ini",
                };
                println!("{}\t{file}\t{}\t{}", entry.category.id(), entry.key, entry.value);
            }
        }
        SettingsCmd::Get { title, key } => match per_game::get(root, &Target::parse(root, &title)?, &key)? {
            Some(value) => println!("{value}"),
            None => eprintln!("{}", Text::new("ps.settings_cli.unset").arg("key", &key)),
        },
        SettingsCmd::Set { title, key, value } => {
            let target = Target::parse(root, &title)?;
            if !value.is_empty() && per_game::is_ini_key(&key) {
                note_shape(&key, per_game::ini_check(&key, found.listed.as_deref())?);
            }
            per_game::set(root, &target, &key, &value)?;
            println!(
                "{}",
                Text::new("ps.settings_cli.set").arg("key", &key).arg("title", &title)
            );
        }
        SettingsCmd::Copy {
            from,
            title: Some(title),
            categories,
            yes,
            ..
        } => copy(root, &from, &title, categories, yes)?,
        SettingsCmd::Copy {
            from, categories, yes, ..
        } => copy_to_all(root, &from, &found.games, categories, yes)?,
        SettingsCmd::Forget { title, yes } => {
            let target = Target::parse(root, &title)?;
            if !yes {
                println!("{}", Text::new("ps.settings_cli.forget_dry_run").arg("title", &title));
                return Ok(());
            }
            let backup = per_game::forget(root, &target)?;
            println!("{}", Text::new("ps.settings_cli.forgotten").arg("title", &title));
            print_backup(&backup);
        }
        SettingsCmd::Reset { title, yes } => {
            let target = Target::parse(root, &title)?;
            if !yes {
                println!("{}", Text::new("ps.settings_cli.reset_dry_run").arg("title", &title));
                return Ok(());
            }
            let backup = per_game::reset(root, &target)?;
            println!("{}", Text::new("ps.settings_cli.reset").arg("title", &title));
            print_backup(&backup);
        }
        SettingsCmd::Export { title, out } => {
            let text = transfer::export(root, &Target::parse(root, &title)?)?;
            write_new(&out, text.as_bytes())?;
            println!(
                "{}",
                Text::new("ps.settings_cli.exported")
                    .arg("title", &title)
                    .arg("path", out.display())
            );
        }
        SettingsCmd::Import {
            title,
            file,
            any_title,
            yes,
        } => {
            let target = Target::parse(root, &title)?;
            transfer::check_file(root, &target, &file, any_title)?;
            note_switches(&transfer::ini_keys(&file)?, found.listed.as_deref());
            if !yes {
                println!("{}", Text::new("ps.settings_cli.import_dry_run").arg("title", &title));
                return Ok(());
            }
            let backup = transfer::import_file(root, &target, &file, any_title)?;
            println!("{}", Text::new("ps.settings_cli.imported").arg("title", &title));
            print_backup(&backup);
        }
    }
    Ok(())
}

/// Says on stderr when nothing but `key`'s shape could be checked, so a typo isn't taken for a
/// setting the port reads.
fn note_shape(key: &str, check: IniCheck) {
    if check != IniCheck::Shape {
        return;
    }
    let why = if per_game::is_effect_key(key) {
        "ps.settings_cli.shape_only_unlisted"
    } else {
        "ps.settings_cli.shape_only"
    };
    eprintln!("{}", Text::new(why).arg("key", key));
}

/// An imported file's switches the port doesn't list for the title are said, not refused: a title's
/// ini began as the shared one, which carries Bloodborne's switches for every game, so a game's own
/// export would be refused.
fn note_switches(keys: &[String], listed: Option<&[String]>) {
    let unlisted: Vec<&str> = keys
        .iter()
        .filter(|key| matches!(per_game::ini_check(key, listed), Err(Refusal::NotListed(_))))
        .map(String::as_str)
        .collect();
    let note = if !unlisted.is_empty() {
        Some(Text::new("ps.settings_cli.import_unlisted").arg("keys", unlisted.join(", ")))
    } else if listed.is_none() && keys.iter().any(|key| per_game::is_effect_key(key)) {
        Some(Text::new("ps.settings_cli.import_shape_only"))
    } else {
        None
    };
    if let Some(note) = note {
        eprintln!("{note}");
    }
}

fn all_or(categories: Vec<Category>) -> (Vec<Category>, String) {
    let categories = if categories.is_empty() {
        Category::ALL.to_vec()
    } else {
        categories
    };
    let names = categories
        .iter()
        .map(|category| category.id())
        .collect::<Vec<_>>()
        .join(", ");
    (categories, names)
}

fn copy_to_all(
    root: &Root,
    from: &str,
    games: &[(String, PathBuf)],
    categories: Vec<Category>,
    yes: bool,
) -> Result<(), PerGameError> {
    let (categories, names) = all_or(categories);
    let source = Target::parse(root, from)?;
    let plan = per_game::plan_to_all(root, &source, games, &categories)?;
    let reached = plan
        .iter()
        .filter(|destination| !destination.categories.is_empty())
        .count();
    let without_online: Vec<&str> = plan
        .iter()
        .filter(|destination| destination.is_without_online)
        .map(|destination| destination.title_id.as_str())
        .collect();
    let summary = Text::new(if yes {
        "ps.settings_cli.copy_all"
    } else {
        "ps.settings_cli.copy_all_dry_run"
    })
    .arg("from", from)
    .arg("count", reached)
    .arg("categories", &names);
    println!("{summary}");
    if !without_online.is_empty() {
        println!(
            "{}",
            Text::new("ps.settings_cli.copy_all_without_online").arg("titles", without_online.join(", "))
        );
    }
    if !yes {
        for destination in plan.iter().filter(|destination| !destination.categories.is_empty()) {
            println!("{}", destination.title_id);
        }
        return Ok(());
    }
    let mut failed = None;
    for applied in per_game::copy_to_all(root, &source, &plan)? {
        match applied.result {
            Ok(backup) => println!(
                "{}",
                Text::new("ps.settings_cli.copy_all_done")
                    .arg("title", &applied.title_id)
                    .arg("path", backup.display())
            ),
            Err(err) => {
                eprintln!(
                    "{}",
                    Text::new("ps.settings_cli.copy_all_failed")
                        .arg("title", &applied.title_id)
                        .text("error", per_game_error(&err))
                );
                failed.get_or_insert(err);
            }
        }
    }
    failed.map_or(Ok(()), Err)
}

fn copy(root: &Root, from: &str, title: &str, categories: Vec<Category>, yes: bool) -> Result<(), PerGameError> {
    let (categories, names) = all_or(categories);
    let (source, destination) = (Target::parse(root, from)?, Target::parse(root, title)?);
    if !yes {
        print_done("ps.settings_cli.copy_dry_run", from, title, &names);
        return Ok(());
    }
    let backup = per_game::copy(root, &source, &destination, &categories)?;
    print_done("ps.settings_cli.copied", from, title, &names);
    print_backup(&backup);
    Ok(())
}

fn print_done(key: &'static str, from: &str, title: &str, categories: &str) {
    println!(
        "{}",
        Text::new(key)
            .arg("from", from)
            .arg("title", title)
            .arg("categories", categories)
    );
}

/// The backup is inside the library, the only kind of path printed besides the one given.
fn print_backup(backup: &Path) {
    println!("{}", Text::new("ps.settings_cli.backup").arg("path", backup.display()));
}

fn write_new(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}
