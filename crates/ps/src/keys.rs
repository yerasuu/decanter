//! GPUI key names to SDL scancode names, the names both ports pass to `SDL_GetScancodeFromName`.
//!
//! GPUI reports the character on the key, SDL binds the physical position (US layout names).
//! They agree for letters, digits and named keys on most layouts; shifted symbols are folded
//! back to their US key, and anything else is reported as unsupported rather than guessed.

/// The SDL name for a GPUI key, or `None` when there is no reliable equivalent.
pub fn sdl_key_name(key: &str) -> Option<String> {
    let named = match key {
        "enter" => "Return",
        "escape" => "Escape",
        "backspace" => "Backspace",
        "tab" => "Tab",
        "space" => "Space",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        "-" | "_" => "-",
        "=" | "+" => "=",
        "[" | "{" => "[",
        "]" | "}" => "]",
        "\\" | "|" => "\\",
        ";" | ":" => ";",
        "'" | "\"" => "'",
        "`" | "~" => "`",
        "," | "<" => ",",
        "." | ">" => ".",
        "/" | "?" => "/",
        "!" => "1",
        "@" => "2",
        "#" => "3",
        "$" => "4",
        "%" => "5",
        "^" => "6",
        "&" => "7",
        "*" => "8",
        "(" => "9",
        ")" => "0",
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_owned());
    }
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && c.is_ascii_alphanumeric()
    {
        return Some(c.to_ascii_uppercase().to_string());
    }
    if let Some(number) = key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
        && (1..=24).contains(&number)
    {
        return Some(format!("F{number}"));
    }
    None
}
