//! `decanter ps settings`: a game's own settings, or the defaults new games copy, from the command
//! line, through the same operations as a game's page and the defaults page
//! ([`decanter_ps::bbport::per_game`], [`decanter_ps::bbport::transfer`]), for night scripts and
//! Windows tests. Nothing that replaces a game's settings happens without `--yes`; without it the
//! command says what it would do.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use decanter_core::Db;
use decanter_ps::bbport::per_game::{self, Category, DEFAULTS, PerGameError, Target};
use decanter_ps::bbport::{Store, transfer};
use decanter_ps::paths::{self, Root};
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
    /// Copies another game's settings over this one's: all of them, or each --category given
    /// (graphics, keyboard, gamepad, mods, online, extras). Needs --yes.
    Copy {
        #[arg(long, value_parser = target)]
        from: String,
        #[arg(long, value_parser = target)]
        title: String,
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

/// The process's exit code: 0 done or shown, [`FAILED`], or [`REFUSED`].
pub async fn run(db: &Db, cmd: SettingsCmd) -> i32 {
    let root = match library_root(db) {
        Ok(root) => root,
        Err(why) => {
            eprintln!("error: {why}");
            return REFUSED;
        }
    };
    let done = tokio::task::spawn_blocking(move || execute(&root, cmd)).await;
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

fn library_root(db: &Db) -> Result<Root, Text> {
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
    Ok(paths::active())
}

fn execute(root: &Root, cmd: SettingsCmd) -> Result<(), PerGameError> {
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
            per_game::set(root, &Target::parse(root, &title)?, &key, &value)?;
            println!(
                "{}",
                Text::new("ps.settings_cli.set").arg("key", &key).arg("title", &title)
            );
        }
        SettingsCmd::Copy {
            from,
            title,
            categories,
            yes,
        } => copy(root, &from, &title, categories, yes)?,
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

fn copy(root: &Root, from: &str, title: &str, categories: Vec<Category>, yes: bool) -> Result<(), PerGameError> {
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
