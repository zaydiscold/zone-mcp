//! What an MCP server lets an agent see.
//!
//! Every registered agent could otherwise read the whole library, and most of a library is
//! dictations typed by voice into other apps: private messages. A scope narrows one server:
//! to some kinds (`--kinds note,meeting`), and away from dictations spoken in some places
//! (`--exclude-sources keyboard`). `zone mcp install <client>` takes the same flags, so each
//! client can be registered with its own view.
//!
//! The scope is part of every query the tools make ([`crate::db::Filter`]), so an item outside
//! it cannot be listed, searched, read, counted or flagged, and the server's instructions tell
//! the agent what it is not seeing. The default is everything.
//!
//! This is a limit on what leaves the computer through one server. It is not a wall between
//! programs: anything running as the same user can still open the library file.

use anyhow::{Result, bail};
use clap::Args;

/// The kinds of item, in the order they are always written.
pub const KINDS: [&str; 3] = ["dictation", "note", "meeting"];

/// The view flags of `zone mcp` and `zone mcp install`.
#[derive(Args, Clone, Debug, Default)]
pub struct ViewArgs {
    /// Leave out mark_processed, the one tool that writes
    #[arg(long)]
    pub read_only: bool,
    /// Show agents only these kinds: dictation, note, meeting (comma separated), or all
    #[arg(long, value_name = "KINDS", value_delimiter = ',')]
    pub kinds: Vec<String>,
    /// Hide dictations spoken in these places: keyboard (typed by voice in other apps), app,
    /// note (comma separated), or none
    #[arg(long, value_name = "SOURCES", value_delimiter = ',')]
    pub exclude_sources: Vec<String>,
    /// Show agents only your voice notes. The same as --kinds note
    #[arg(long, conflicts_with = "kinds")]
    pub notes_only: bool,
    /// Hide what you typed by voice into other apps (messages, emails, searches). The same as
    /// --exclude-sources keyboard
    #[arg(long)]
    pub no_keyboard: bool,
}

impl ViewArgs {
    /// True when a flag about what agents see was given, even one that asks for everything.
    pub fn names_a_view(&self) -> bool {
        !self.kinds.is_empty() || !self.exclude_sources.is_empty() || self.notes_only || self.no_keyboard
    }

    pub fn scope(&self) -> Result<Scope> {
        let mut kinds = self.kinds.clone();
        if self.notes_only {
            kinds.push("note".to_string());
        }
        let mut sources = self.exclude_sources.clone();
        if self.no_keyboard {
            sources.push("keyboard".to_string());
        }
        Scope::new(&kinds, &sources)
    }

    /// These flags, with any the other set adds. For `zone mcp --read-only install <client>`,
    /// where a flag may come before or after `install`.
    pub fn or(&self, outer: &ViewArgs) -> ViewArgs {
        if self.names_a_view() {
            ViewArgs { read_only: self.read_only || outer.read_only, ..self.clone() }
        } else {
            ViewArgs { read_only: self.read_only || outer.read_only, ..outer.clone() }
        }
    }
}

/// What a server shows. The default shows everything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    /// The kinds that can be seen, in the order of [`KINDS`]. Empty means all of them.
    pub kinds: Vec<String>,
    /// Dictations whose source is one of these are hidden. Lowercase, sorted.
    pub exclude_sources: Vec<String>,
}

fn kind_name(kind: &str) -> Option<&'static str> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "dictation" | "dictations" => Some("dictation"),
        "note" | "notes" => Some("note"),
        "meeting" | "meetings" => Some("meeting"),
        _ => None,
    }
}

fn plural(kind: &str) -> &'static str {
    match kind {
        "dictation" => "dictations",
        "note" => "voice notes",
        _ => "meeting transcripts",
    }
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

impl Scope {
    /// A scope from what a person typed. A kind zone does not know is an error rather than a
    /// guess: a mistyped limit must never turn into no limit.
    pub fn new(kinds: &[String], exclude_sources: &[String]) -> Result<Scope> {
        let mut seen = [false; 3];
        let mut all = false;
        for kind in kinds.iter().map(|k| k.trim()).filter(|k| !k.is_empty()) {
            if kind.eq_ignore_ascii_case("all") {
                all = true;
                continue;
            }
            match kind_name(kind).and_then(|name| KINDS.iter().position(|k| *k == name)) {
                Some(index) => seen[index] = true,
                None => bail!("--kinds takes dictation, note, meeting (or all), not \"{kind}\""),
            }
        }
        let kinds: Vec<String> =
            if all || seen.iter().all(|s| *s) { Vec::new() } else { KINDS.iter().zip(seen).filter(|(_, s)| *s).map(|(k, _)| k.to_string()).collect() };
        let mut sources: Vec<String> = exclude_sources.iter().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty() && s != "none").collect();
        sources.sort();
        sources.dedup();
        if let Some(odd) = sources.iter().find(|s| s.starts_with('-') || s.chars().any(char::is_whitespace)) {
            bail!("--exclude-sources takes source names such as keyboard, app or note, not \"{odd}\"");
        }
        Ok(Scope { kinds, exclude_sources: sources })
    }

    pub fn is_everything(&self) -> bool {
        self.kinds.is_empty() && self.exclude_sources.is_empty()
    }

    /// The kinds that can be seen, spelled out.
    pub fn visible_kinds(&self) -> Vec<&str> {
        if self.kinds.is_empty() { KINDS.to_vec() } else { self.kinds.iter().map(String::as_str).collect() }
    }

    pub fn allows_kind(&self, kind: &str) -> bool {
        self.kinds.is_empty() || self.kinds.iter().any(|k| k == kind)
    }

    pub fn hides_source(&self, source: &str) -> bool {
        let source = source.trim().to_lowercase();
        self.exclude_sources.contains(&source)
    }

    /// Whether an item of this kind and source can be seen.
    pub fn allows(&self, kind: &str, source: Option<&str>) -> bool {
        self.allows_kind(kind) && !source.is_some_and(|s| self.hides_source(s))
    }

    /// The flags that start a server with this scope, in one fixed spelling.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if !self.kinds.is_empty() {
            args.push("--kinds".to_string());
            args.push(self.kinds.join(","));
        }
        if !self.exclude_sources.is_empty() {
            args.push("--exclude-sources".to_string());
            args.push(self.exclude_sources.join(","));
        }
        args
    }

    /// Reads the scope back out of the arguments a client starts the server with, however
    /// they were spelled. None when they cannot be understood.
    pub fn from_launch_args(args: &[String]) -> Option<Scope> {
        let mut kinds: Vec<String> = Vec::new();
        let mut sources: Vec<String> = Vec::new();
        let mut index = 0;
        let split = |text: &str| -> Vec<String> { text.split(',').map(str::to_string).collect() };
        while index < args.len() {
            let arg = args[index].as_str();
            index += 1;
            let (flag, inline) = match arg.split_once('=') {
                Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
                _ => (arg, None),
            };
            // The value of a flag: after `=`, or the next argument.
            let value = |index: &mut usize| -> Option<String> {
                inline.clone().or_else(|| {
                    *index += 1;
                    args.get(*index - 1).cloned()
                })
            };
            match flag {
                "--kinds" => kinds.extend(split(&value(&mut index)?)),
                "--exclude-sources" => sources.extend(split(&value(&mut index)?)),
                "--notes-only" => kinds.push("note".to_string()),
                "--no-keyboard" => sources.push("keyboard".to_string()),
                _ => {}
            }
        }
        Scope::new(&kinds, &sources).ok()
    }

    /// A few words for a status line: `only voice notes and meeting transcripts, hides keyboard`.
    /// None when everything is shown.
    pub fn short(&self) -> Option<String> {
        if self.is_everything() {
            return None;
        }
        let mut parts = Vec::new();
        if !self.kinds.is_empty() {
            let names: Vec<String> = self.kinds.iter().map(|k| plural(k).to_string()).collect();
            parts.push(format!("only {}", and_list(&names)));
        }
        if !self.exclude_sources.is_empty() {
            parts.push(format!("hides {}", self.exclude_sources.join(", ")));
        }
        Some(parts.join(", "))
    }

    /// A sentence or two for the agent: what this server shows and what it leaves out. None
    /// when everything is shown.
    pub fn for_agents(&self) -> Option<String> {
        if self.is_everything() {
            return None;
        }
        let mut shows = Vec::new();
        if !self.kinds.is_empty() {
            let names: Vec<String> = self.kinds.iter().map(|k| plural(k).to_string()).collect();
            let hidden: Vec<String> = KINDS.iter().filter(|k| !self.allows_kind(k)).map(|k| plural(k).to_string()).collect();
            shows.push(format!("it shows only {} ({} are hidden)", and_list(&names), and_list(&hidden)));
        }
        // Dictations are the only items with a source, so this says nothing when they are hidden anyway.
        if !self.exclude_sources.is_empty() && self.allows_kind("dictation") {
            let names: Vec<String> = self
                .exclude_sources
                .iter()
                .map(|s| if s == "keyboard" { "`keyboard` (what they typed by voice into other apps)".to_string() } else { format!("`{s}`") })
                .collect();
            shows.push(format!("it hides dictations whose source is {}", and_list(&names)));
        }
        if shows.is_empty() {
            return None;
        }
        Some(format!(
            "This server has a limited view, chosen by the person when they connected it: {}. Hidden items cannot be listed, searched, \
             read or counted through any tool here, so totals describe only what you can see, not the whole library. If a request \
             needs something hidden, say that this connection does not include it. The person can widen it with `zone mcp install`.",
            shows.join(", and ")
        ))
    }

    /// A sentence for the person who just registered a server with this scope.
    pub fn for_people(&self) -> Option<String> {
        let short = self.short()?;
        Some(format!("Limited view for agents there: {short}."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn scopes_are_written_one_way_and_read_back() {
        assert!(Scope::default().is_everything());
        assert_eq!(Scope::default().to_args(), Vec::<String>::new());
        assert_eq!(Scope::default().visible_kinds(), KINDS);
        assert_eq!((Scope::default().short(), Scope::default().for_agents()), (None, None));

        // Any order and spelling becomes the one order.
        let scope = Scope::new(&list(&["Meetings", " note"]), &list(&["Keyboard", "keyboard", "app"])).unwrap();
        assert_eq!((scope.kinds.clone(), scope.exclude_sources.clone()), (list(&["note", "meeting"]), list(&["app", "keyboard"])));
        assert_eq!(scope.to_args(), list(&["--kinds", "note,meeting", "--exclude-sources", "app,keyboard"]));
        assert_eq!(Scope::from_launch_args(&[list(&["mcp", "--read-only"]), scope.to_args()].concat()), Some(scope.clone()));
        assert_eq!(scope.short().unwrap(), "only voice notes and meeting transcripts, hides app, keyboard");

        // All three kinds, or `all`, is no limit. `none` hides no source.
        assert!(Scope::new(&list(&["note", "meeting", "dictation"]), &[]).unwrap().is_everything());
        assert!(Scope::new(&list(&["all"]), &list(&["none"])).unwrap().is_everything());
        // A typo is refused, never read as "everything".
        assert!(Scope::new(&list(&["notse"]), &[]).unwrap_err().to_string().contains("notse"));
        assert!(Scope::new(&[], &list(&["--kinds"])).is_err());

        // The other spellings a config file may hold.
        assert_eq!(Scope::from_launch_args(&list(&["mcp", "--notes-only"])).unwrap().kinds, list(&["note"]));
        assert_eq!(Scope::from_launch_args(&list(&["mcp", "--kinds=note,meeting", "--no-keyboard"])).unwrap(), Scope { kinds: list(&["note", "meeting"]), exclude_sources: list(&["keyboard"]) });
        assert_eq!(Scope::from_launch_args(&list(&["mcp"])), Some(Scope::default()));
        assert_eq!(Scope::from_launch_args(&list(&["mcp", "--kinds", "songs"])), None);
        assert_eq!(Scope::from_launch_args(&list(&["mcp", "--kinds"])), None);
    }

    #[test]
    fn what_a_scope_lets_through() {
        let scope = Scope::new(&[], &list(&["keyboard"])).unwrap();
        assert!(scope.allows("dictation", Some("app")) && scope.allows("note", None) && scope.allows("dictation", None));
        assert!(!scope.allows("dictation", Some("keyboard")) && !scope.allows("dictation", Some("Keyboard ")));
        let notes = Scope::new(&list(&["note"]), &[]).unwrap();
        assert!(notes.allows("note", None) && !notes.allows("dictation", Some("note")) && !notes.allows("meeting", None));

        let words = scope.for_agents().unwrap();
        assert!(words.contains("limited view") && words.contains("`keyboard` (what they typed by voice into other apps)") && words.contains("not the whole library"), "{words}");
        let words = Scope::new(&list(&["note", "meeting"]), &list(&["keyboard"])).unwrap().for_agents().unwrap();
        // With dictations hidden altogether, their sources need no mention.
        assert!(words.contains("only voice notes and meeting transcripts (dictations are hidden)") && !words.contains("keyboard"), "{words}");
        assert_eq!(notes.for_people().unwrap(), "Limited view for agents there: only voice notes.");
        assert!(!words.contains('\u{2014}'));
    }

    #[test]
    fn flags_become_a_scope() {
        let args = ViewArgs { notes_only: true, no_keyboard: true, ..ViewArgs::default() };
        assert!(args.names_a_view());
        assert_eq!(args.scope().unwrap(), Scope { kinds: list(&["note"]), exclude_sources: list(&["keyboard"]) });
        assert!(!ViewArgs { read_only: true, ..ViewArgs::default() }.names_a_view());
        // `--kinds all` names a view (everything) without limiting anything.
        let everything = ViewArgs { kinds: list(&["all"]), ..ViewArgs::default() };
        assert!(everything.names_a_view() && everything.scope().unwrap().is_everything());
        // A flag before `install` counts when none comes after it.
        let outer = ViewArgs { read_only: true, kinds: list(&["note"]), ..ViewArgs::default() };
        let merged = ViewArgs::default().or(&outer);
        assert!(merged.read_only && merged.scope().unwrap().kinds == list(&["note"]));
        let inner = ViewArgs { kinds: list(&["meeting"]), ..ViewArgs::default() };
        assert_eq!(inner.or(&outer).scope().unwrap().kinds, list(&["meeting"]));
    }
}
