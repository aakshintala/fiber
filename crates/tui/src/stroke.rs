//! A keystroke: one key and its modifiers (`docs/tui.md`, "Input and focus").
//!
//! Key names read case-insensitively as `mod+...+key`, with the modifiers in
//! any order; an uppercase letter reads as `shift` with that letter, and
//! `shift` on a character that is not a letter is dropped. The written form
//! is lowercase with the modifiers in `ctrl`, `shift`, `alt`, `super` order;
//! the shown form follows the bindings table's style.

/// The modifiers held with a stroke: Ctrl, Shift, Alt (option) and Super
/// (command).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) struct Mods(u8);

impl Mods {
    /// No modifiers.
    pub(crate) const NONE: Mods = Mods(0);
    /// Ctrl.
    pub(crate) const CTRL: Mods = Mods(1);
    /// Shift.
    pub(crate) const SHIFT: Mods = Mods(2);
    /// Alt (option).
    pub(crate) const ALT: Mods = Mods(4);
    /// Super (command).
    pub(crate) const SUPER: Mods = Mods(8);

    /// Whether every modifier in `other` is held.
    pub(crate) fn contains(self, other: Mods) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Mods {
    type Output = Mods;

    fn bitor(self, rhs: Mods) -> Mods {
        Mods(self.0 | rhs.0)
    }
}

/// One key without its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Code {
    /// A printable character: never a control character, never an uppercase
    /// letter (that shift lives in [`Mods`]), and never `' '` (that is
    /// [`Code::Space`]).
    Char(char),
    /// Enter.
    Enter,
    /// Escape.
    Esc,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// Insert.
    Insert,
    /// Home.
    Home,
    /// End.
    End,
    /// PageUp.
    PageUp,
    /// PageDown.
    PageDown,
    /// Up.
    Up,
    /// Down.
    Down,
    /// Left.
    Left,
    /// Right.
    Right,
    /// The space bar.
    Space,
    /// F1 to F12: the number.
    F(u8),
}

/// One key and its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Stroke {
    /// The key.
    pub(crate) code: Code,
    /// The modifiers held with it.
    pub(crate) mods: Mods,
}

impl Stroke {
    // debt: Part 1a tests only; Part 1b's keyset consumes key names.
    // Upgrade trigger: Part 1b.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "debt: Part 1a tests only; Part 1b's keyset consumes key names."
        )
    )]
    /// Reads a key name such as `ctrl+t`, `alt+up` or `shift+enter`:
    /// case-insensitive, the modifiers in any order, `control` for `ctrl`,
    /// `opt`, `option` or `meta` for `alt`, `cmd` or `command` for `super`,
    /// `return` for `enter`, `escape` for `esc`, and `f1` to `f12`. A lone
    /// `+` names the plus key. An uppercase letter reads as `shift` with
    /// that letter, and `shift` on a character that is not a letter is
    /// dropped, so `shift+?` is `?`. A repeated modifier counts once.
    pub(crate) fn parse(text: &str) -> Result<Stroke, String> {
        let invalid = || format!("{text:?} is not a key name");
        // The `+` key alone: splitting on `+` would leave nothing to name it.
        if text == "+" {
            return Ok(Stroke {
                code: Code::Char('+'),
                mods: Mods::NONE,
            });
        }
        let mut parts = text.split('+');
        let mut key_name = parts.next_back().ok_or_else(invalid)?;
        // `ctrl++` names Ctrl with the plus key: the split leaves the key
        // empty, so the plus before the separator is the key instead.
        // `ctrl+` still names nothing.
        if key_name.is_empty() && text.ends_with("++") {
            let separator = parts.next_back().ok_or_else(invalid)?;
            if !separator.is_empty() {
                return Err(invalid());
            }
            key_name = "+";
        }
        let mut mods = Mods::NONE;
        for name in parts {
            match name.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods = mods | Mods::CTRL,
                "shift" => mods = mods | Mods::SHIFT,
                "alt" | "opt" | "option" | "meta" => mods = mods | Mods::ALT,
                "super" | "cmd" | "command" => mods = mods | Mods::SUPER,
                _ => return Err(invalid()),
            }
        }
        let lower = key_name.to_ascii_lowercase();
        let code = match lower.as_str() {
            "enter" | "return" => Code::Enter,
            "esc" | "escape" => Code::Esc,
            "tab" => Code::Tab,
            "space" => Code::Space,
            "backspace" => Code::Backspace,
            "delete" => Code::Delete,
            "insert" => Code::Insert,
            "home" => Code::Home,
            "end" => Code::End,
            "pageup" => Code::PageUp,
            "pagedown" => Code::PageDown,
            "up" => Code::Up,
            "down" => Code::Down,
            "left" => Code::Left,
            "right" => Code::Right,
            _ => {
                if let Some(n) = function_key(&lower) {
                    Code::F(n)
                } else {
                    let mut chars = key_name.chars();
                    let (first, second) = (chars.next(), chars.next());
                    let Some(ch) = first else {
                        return Err(invalid());
                    };
                    if second.is_some() {
                        return Err(invalid());
                    }
                    if ch.is_control() {
                        return Err(invalid());
                    }
                    if ch == ' ' {
                        Code::Space
                    } else {
                        let mut folded = ch.to_lowercase();
                        match (folded.next(), folded.next()) {
                            (Some(one), None) if one != ch => {
                                // Fold only when shift round-trips: the
                                // uppercase of the lowercase is the letter
                                // again, so `name` and the shown form agree
                                // on it (`ß` stays `ß`, `ẞ` stays `ẞ`).
                                let mut back = one.to_uppercase();
                                if (back.next(), back.next()) == (Some(ch), None) {
                                    mods = mods | Mods::SHIFT;
                                    Code::Char(one)
                                } else {
                                    Code::Char(ch)
                                }
                            }
                            _ => Code::Char(ch),
                        }
                    }
                }
            }
        };
        if let Code::Char(ch) = code
            && !ch.is_alphabetic()
            && mods.contains(Mods::SHIFT)
        {
            mods = Mods(mods.0 & !Mods::SHIFT.0);
        }
        Ok(Stroke { code, mods })
    }

    // debt: Part 1a tests only; Part 1b's keyset consumes key names.
    // Upgrade trigger: Part 1b.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "debt: Part 1a tests only; Part 1b's keyset consumes key names."
        )
    )]
    /// The written form: lowercase, the modifiers in `ctrl`, `shift`, `alt`,
    /// `super` order, such as `ctrl+t`.
    pub(crate) fn name(&self) -> String {
        let mut text = String::new();
        for (held, word) in [
            (Mods::CTRL, "ctrl"),
            (Mods::SHIFT, "shift"),
            (Mods::ALT, "alt"),
            (Mods::SUPER, "super"),
        ] {
            if self.mods.contains(held) {
                text.push_str(word);
                text.push('+');
            }
        }
        text.push_str(&key_name(self.code));
        text
    }

    // debt: Part 1a tests only; Part 1b's keyset consumes key names.
    // Upgrade trigger: Part 1b.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "debt: Part 1a tests only; Part 1b's keyset consumes key names."
        )
    )]
    /// The shown form in the bindings table's style: `Ctrl+T`, `⌥P`,
    /// `Shift+Enter`, `⌘←`, `↑`, `F1`, `y`.
    pub(crate) fn label(&self) -> String {
        let mut text = String::new();
        if self.mods.contains(Mods::CTRL) {
            text.push_str("Ctrl+");
        }
        if self.mods.contains(Mods::SHIFT) && !self.bare_shifted_letter() {
            text.push_str("Shift+");
        }
        if self.mods.contains(Mods::ALT) {
            text.push('⌥');
        }
        if self.mods.contains(Mods::SUPER) {
            text.push('⌘');
        }
        text.push_str(&self.base_label());
        text
    }

    /// Whether the stroke is a shifted letter alone, which shows as the
    /// uppercase letter with no `Shift` prefix.
    fn bare_shifted_letter(&self) -> bool {
        matches!(self.code, Code::Char(ch) if ch.is_alphabetic()) && self.mods == Mods::SHIFT
    }

    /// The shown form of the key without its modifiers.
    fn base_label(&self) -> String {
        match self.code {
            Code::Char(ch) => {
                if self.mods == Mods::NONE || !ch.is_alphabetic() {
                    ch.to_string()
                } else {
                    ch.to_uppercase()
                        .next()
                        .map_or_else(|| ch.to_string(), String::from)
                }
            }
            Code::Enter => "Enter".to_owned(),
            Code::Esc => "Esc".to_owned(),
            Code::Tab => "Tab".to_owned(),
            Code::Backspace => "Backspace".to_owned(),
            Code::Delete => "Delete".to_owned(),
            Code::Insert => "Insert".to_owned(),
            Code::Home => "Home".to_owned(),
            Code::End => "End".to_owned(),
            Code::PageUp => "PageUp".to_owned(),
            Code::PageDown => "PageDown".to_owned(),
            Code::Up => "↑".to_owned(),
            Code::Down => "↓".to_owned(),
            Code::Left => "←".to_owned(),
            Code::Right => "→".to_owned(),
            Code::Space => "Space".to_owned(),
            Code::F(n) => format!("F{n}"),
        }
    }
}

/// The written form of a key without its modifiers.
fn key_name(code: Code) -> String {
    match code {
        Code::Char(ch) => ch.to_string(),
        Code::Enter => "enter".to_owned(),
        Code::Esc => "esc".to_owned(),
        Code::Tab => "tab".to_owned(),
        Code::Backspace => "backspace".to_owned(),
        Code::Delete => "delete".to_owned(),
        Code::Insert => "insert".to_owned(),
        Code::Home => "home".to_owned(),
        Code::End => "end".to_owned(),
        Code::PageUp => "pageup".to_owned(),
        Code::PageDown => "pagedown".to_owned(),
        Code::Up => "up".to_owned(),
        Code::Down => "down".to_owned(),
        Code::Left => "left".to_owned(),
        Code::Right => "right".to_owned(),
        Code::Space => "space".to_owned(),
        Code::F(n) => format!("f{n}"),
    }
}

/// The number of `f<n>`, or `None` outside F1 to F12. An empty or
/// non-digit tail is none: the caller reads a lone `f` as the letter.
fn function_key(name: &str) -> Option<u8> {
    let digits = name.strip_prefix('f')?;
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let n: u8 = digits.parse().ok()?;
    (1..=12).contains(&n).then_some(n)
}

#[cfg(test)]
#[path = "stroke_tests.rs"]
mod tests;
