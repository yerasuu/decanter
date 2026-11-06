// SPDX-License-Identifier: GPL-3.0-or-later
//! A game's `fuel.json`, what Amazon's launcher starts it with (stage S5 of
//! docs/amazon-nile-integration-analysis.md, section 5.1): `Main.Command`, `Main.Args` and
//! `Main.WorkingSubdirOverride`, relative to the game's folder.
//!
//! nile reads it with json5. Amazon documents neither the file nor which json5 features games use,
//! so the two the analysis names, comments and trailing commas, are taken out before `serde_json`
//! reads it, rather than adding a json5 parser for them; anything else json5 allows fails visibly.
//! The file ships with the game and is untrusted: the program and the folder are checked as a
//! manifest's paths are, and on disk they must be a file and a folder inside the game's, reached
//! through no symbolic link.

use std::io::Read as _;
use std::path::Path;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::Value;

use super::disk;
use crate::manifest::{ManifestPath, PathFault};

const FILE: &str = "fuel.json";
/// Past any launch description; caps what a hostile file makes decanter read.
const MAX_LEN: u64 = 1 << 20;
const MAX_ARGS: usize = 256;
const ARG_MAX_LEN: usize = 4096;

/// How the game asks to be started.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FuelLaunch {
    /// The program, relative to the game's folder.
    pub command: ManifestPath,
    pub args: Vec<String>,
    /// The folder the game starts in, relative to its own; `None` starts it in the game's folder,
    /// as nile does, not in the program's.
    pub working_subdir: Option<ManifestPath>,
}

#[derive(Deserialize)]
struct Fuel {
    #[serde(default, rename = "SchemaVersion")]
    schema_version: Option<Value>,
    #[serde(rename = "Main")]
    main: Main,
}

#[derive(Deserialize)]
struct Main {
    #[serde(rename = "Command")]
    command: String,
    #[serde(default, rename = "Args")]
    args: Vec<String>,
    #[serde(default, rename = "WorkingSubdirOverride")]
    working_subdir: Option<String>,
}

/// Reads and checks `<install_dir>/fuel.json`. Blocking, though only a small read and a few `lstat`s.
///
/// # Errors
///
/// When the file is missing or larger than a mebibyte, is not the launch description expected, or names
/// a program or a folder that is not inside the game's folder.
#[inline]
pub fn read_fuel(install_dir: &Path) -> anyhow::Result<FuelLaunch> {
    let path = install_dir.join(FILE);
    let meta = std::fs::symlink_metadata(&path).with_context(|| format!("reading {}", path.display()))?;
    anyhow::ensure!(meta.file_type().is_file(), "{} is not a regular file", path.display());
    anyhow::ensure!(
        meta.len() <= MAX_LEN,
        "{} is larger than {MAX_LEN} bytes",
        path.display()
    );
    let mut raw = String::new();
    std::fs::File::open(&path)
        .and_then(|file| file.take(MAX_LEN).read_to_string(&mut raw))
        .with_context(|| format!("reading {}", path.display()))?;
    let text = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    let fuel: Fuel = serde_json::from_str(&plain_json(text))
        .with_context(|| format!("{} is not the launch description expected", path.display()))?;
    tracing::debug!(schema_version = ?fuel.schema_version, path = %path.display(), "read an amazon fuel.json");

    let command = relative(&fuel.main.command)
        .map_err(|fault| anyhow::anyhow!("Main.Command {:?} is refused: {fault}", fuel.main.command))?
        .with_context(|| format!("{} names no program", path.display()))?;
    let program = disk::inside(install_dir, &command).context("Main.Command reaches through a symbolic link")?;
    let is_file = std::fs::symlink_metadata(&program).is_ok_and(|found| found.file_type().is_file());
    anyhow::ensure!(
        is_file,
        "Main.Command {:?} is not a file in the game's folder",
        command.as_str()
    );

    let working_subdir = match fuel.main.working_subdir.as_deref().map(relative) {
        None | Some(Ok(None)) => None,
        Some(Ok(Some(subdir))) => {
            let folder = disk::inside(install_dir, &subdir)
                .context("Main.WorkingSubdirOverride reaches through a symbolic link")?;
            let is_dir = std::fs::symlink_metadata(&folder).is_ok_and(|found| found.file_type().is_dir());
            anyhow::ensure!(
                is_dir,
                "Main.WorkingSubdirOverride {:?} is not a folder in the game's folder",
                subdir.as_str()
            );
            Some(subdir)
        }
        Some(Err(fault)) => anyhow::bail!("Main.WorkingSubdirOverride is refused: {fault}"),
    };

    anyhow::ensure!(
        fuel.main.args.len() <= MAX_ARGS,
        "Main.Args holds more than {MAX_ARGS} arguments"
    );
    // A NUL can't be passed to a process at all, so it would only fail the spawn later.
    anyhow::ensure!(
        fuel.main
            .args
            .iter()
            .all(|arg| arg.len() <= ARG_MAX_LEN && !arg.contains('\0')),
        "Main.Args holds an argument longer than {ARG_MAX_LEN} bytes or with a NUL"
    );
    Ok(FuelLaunch {
        command,
        args: fuel.main.args,
        working_subdir,
    })
}

/// `raw` as a path inside the game's folder, `None` for the folder itself. A leading `.\` and a
/// trailing separator are taken as the folder they plainly mean rather than refused.
fn relative(raw: &str) -> Result<Option<ManifestPath>, PathFault> {
    let mut path = raw.trim();
    while let Some(rest) = path.strip_prefix(".\\").or_else(|| path.strip_prefix("./")) {
        path = rest;
    }
    let trimmed = path.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() || trimmed == "." {
        return Ok(None);
    }
    ManifestPath::parse(trimmed).map(Some)
}

/// `text` with its `//` and `/* */` comments blanked out and every comma that only trails before a
/// `}` or `]` dropped, which is JSON when those were all the json5 in it.
fn plain_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut is_string = false;
    let mut trailing_comma = None;
    while let Some(ch) = chars.next() {
        if is_string {
            out.push(ch);
            match ch {
                '\\' => out.extend(chars.next()),
                '"' => is_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '/' if chars.peek() == Some(&'/') => while chars.next_if(|next| *next != '\n').is_some() {},
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = '\0';
                for next in chars.by_ref() {
                    if previous == '*' && next == '/' {
                        break;
                    }
                    previous = next;
                }
                out.push(' ');
            }
            _ if ch.is_whitespace() => out.push(ch),
            ',' => {
                trailing_comma = Some(out.len());
                out.push(ch);
            }
            '}' | ']' => {
                if let Some(at) = trailing_comma.take() {
                    out.replace_range(at..=at, " ");
                }
                out.push(ch);
            }
            _ => {
                trailing_comma = None;
                is_string = ch == '"';
                out.push(ch);
            }
        }
    }
    out
}
