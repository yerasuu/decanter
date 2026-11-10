//! How much a mod platform's game looks like a library game, by name, so that every provider's
//! picker suggests the same way. Only a plausible match is a suggestion: a game sharing one loose
//! word with the title (a "Chronicles" for "Ysiandii Chronicles") is not, since offering it as one
//! led users to map unrelated games.

use std::collections::HashSet;

/// Words too common in titles to say two games are the same.
const COMMON_WORDS: [&str; 8] = [
    "the",
    "of",
    "and",
    "edition",
    "game",
    "remastered",
    "definitive",
    "deluxe",
];
/// Below this, a name held inside the other is a fragment of it, not the same game.
const MIN_CONTAINED_CHARS: usize = 4;

/// Better first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Likeness {
    /// At least two significant words in common.
    Words,
    /// One starts with the other, or holds it as more than 60 % of its letters.
    Contains,
    /// The same letters and digits.
    Exact,
}

/// How `candidate` looks like `title`, `None` when it does not plausibly name the same game.
#[must_use]
#[inline]
pub fn likeness(title: &str, candidate: &str) -> Option<Likeness> {
    let wanted = squashed(title);
    let known = squashed(candidate);
    if wanted.is_empty() || known.is_empty() {
        return None;
    }
    if wanted == known {
        return Some(Likeness::Exact);
    }
    let (short, long) = if wanted.len() <= known.len() {
        (&wanted, &known)
    } else {
        (&known, &wanted)
    };
    let is_held = short.len() >= MIN_CONTAINED_CHARS
        && (long.starts_with(short.as_str())
            || (long.contains(short.as_str()) && short.len().saturating_mul(5) > long.len().saturating_mul(3)));
    if is_held {
        return Some(Likeness::Contains);
    }
    let shared = significant_words(title)
        .intersection(&significant_words(candidate))
        .count();
    (shared >= 2).then_some(Likeness::Words)
}

/// The best of `likeness` over several names of one candidate (a name and an id, say).
#[must_use]
#[inline]
pub fn best_likeness<'name>(title: &str, names: impl IntoIterator<Item = &'name str>) -> Option<Likeness> {
    names.into_iter().filter_map(|name| likeness(title, name)).max()
}

fn squashed(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn significant_words(text: &str) -> HashSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.chars().count() >= 3 && !COMMON_WORDS.contains(&word.as_str()))
        .collect()
}
