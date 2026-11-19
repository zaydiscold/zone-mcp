//! Things to ask an agent: one built-in list of example prompts, used three ways.
//!
//! - `zone prompts [--json]` and `zone prompts show <id>`, for a person to copy, and for Zone
//!   for Mac to show with a Copy button.
//! - MCP prompts (`prompts/list`, `prompts/get`), which clients that support them offer as
//!   slash commands.
//! - The page's Settings, under "Things to ask".
//!
//! Each prompt names the tools to use and reminds the agent that most of the library is
//! speech, with recognition mistakes. A prompt says which kinds of item it works on, and a
//! server whose view hides all of them does not offer it. One that asks the agent to mark
//! items done says something else on a read-only server, and the kinds a prompt names are only
//! the ones the server shows, so following a prompt never runs into a limit.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::scope::Scope;
use crate::tools::Access;

/// One argument a prompt takes.
pub struct Arg {
    pub name: &'static str,
    pub description: &'static str,
    pub required: bool,
    /// Used when the argument is left out. None means the prompt shows `placeholder` instead
    /// (for a person to fill in) or, over MCP, that the argument is required.
    pub default: Option<&'static str>,
    pub placeholder: &'static str,
}

/// One prompt.
pub struct PromptDef {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// The kinds it works on. It is offered when the view shows at least one of them, and
    /// `{kinds}` in its text becomes the ones the view shows, as a JSON list.
    pub kinds: &'static [&'static str],
    pub args: &'static [Arg],
    /// The text. `{name}` is an argument, `{kinds}` the kinds above, `{done}` the sentence
    /// below that fits the server, `{speech}` the reminder that this is speech.
    pub text: &'static str,
    /// What `{done}` becomes on a server that may flag items, and on a read-only one.
    pub done: Option<(&'static str, &'static str)>,
}

impl PromptDef {
    /// Whether a server with this view offers it.
    pub fn offered(&self, scope: &Scope) -> bool {
        self.kinds.iter().any(|kind| scope.allows_kind(kind))
    }

    pub fn writes(&self) -> bool {
        self.done.is_some()
    }
}

const SPEECH: &str = "Most of my Zone library is speech recognized on my phone, so expect misheard words and fix them from context. Quote me only as much as the task needs.";

const SPOKEN: &[&str] = &["dictation", "note", "meeting"];

const DAY: Arg = Arg {
    name: "day",
    description: "The day or window, in words: today, yesterday afternoon, Wednesday, last Friday, 2026-11-11, Wednesday 12pm to 5pm.",
    required: false,
    default: Some("today"),
    placeholder: "[day]",
};

const TOPIC: Arg = Arg { name: "topic", description: "What to look for, in a few words.", required: true, default: None, placeholder: "[topic]" };

pub const PROMPTS: &[PromptDef] = &[
    PromptDef {
        id: "notes-to-plan",
        title: "Turn my voice notes into a plan",
        description: "Turns your open voice notes into one checklist, then marks the ones it covers as done.",
        kinds: &["note"],
        args: &[],
        text: "Turn my open voice notes into a plan. Call list_items with kinds [\"note\"], unprocessed_only true and order oldest (follow next_cursor until there is none), then get_items for any note marked cut. Write one checklist grouped by theme: one line per action, starting with a verb, with the note's date in brackets. Put questions and ideas that are not actions in a short list at the end. Show me the plan, and once I agree, {done}. {speech}",
        done: Some((
            "call mark_processed with the ids of the notes the plan covers, so they do not come back",
            "list the ids of the notes the plan covers (this connection is read-only, so it cannot mark them done)",
        )),
    },
    PromptDef {
        id: "work-through-notes",
        title: "Work through my notes, newest first",
        description: "Goes through your open voice notes one at a time and marks each done as you go.",
        kinds: &["note"],
        args: &[],
        text: "Let's work through my open voice notes one at a time, newest first. Call list_items with kinds [\"note\"], unprocessed_only true, order newest and limit 1. Show me that note in full (get_items if it is marked cut) with its date, and suggest in one line what to do with it. Wait for my answer. When we are done with it, {done}, and fetch the next one the same way. Stop when there are none left and tell me how many we handled. {speech}",
        done: Some((
            "call mark_processed on it",
            "tell me its id and move on (this connection is read-only, so it cannot mark notes done; use a cursor to step past it)",
        )),
    },
    PromptDef {
        id: "what-did-i-say",
        title: "What did I say on a day",
        description: "A recap of everything you said in a day or a window of time, grouped by topic.",
        kinds: SPOKEN,
        args: &[DAY],
        text: "What did I say {day}? Call list_items with when \"{day}\" and kinds {kinds}, check that its range is the window I mean, and follow next_cursor until there is none. Cards carry the full text when it fits; read any marked cut with get_items. Give me a recap grouped by topic, with times, then anything that sounds like a decision, a promise or a task. Dictations with source keyboard are messages I sent to people: summarize them, do not reproduce them. {speech}",
        done: None,
    },
    PromptDef {
        id: "last-meeting",
        title: "Pick up from my last meeting",
        description: "Summary, decisions and follow-ups from your latest meeting transcript.",
        kinds: &["meeting"],
        args: &[],
        text: "Pick up from my last meeting. Call list_items with kinds [\"meeting\"] and limit 1, then get_items on it and keep reading from next_offset until the transcript is complete. Give me: the meeting's title and time, a three-line summary, the decisions made, follow-ups with who owns each, and open questions. Then draft my follow-up message to the attendees. Speaker names come from the transcript and may be generic (Speaker 1). {speech}",
        done: None,
    },
    PromptDef {
        id: "just-sent",
        title: "Use what I just sent",
        description: "Picks up the newest text you sent from your phone (a clip) and does something with it.",
        kinds: &["clip"],
        args: &[Arg {
            name: "task",
            description: "What to do with it. Left out, the agent asks unless it is obvious.",
            required: false,
            default: Some("If it is not obvious what I want done with it, ask me."),
            placeholder: "[what to do with it]",
        }],
        text: "Use what I just sent you from my phone. Call list_items with kinds [\"clip\"] and limit 1, then get_items on it for the whole text. It is exact text I sent on purpose (a link, an address, a snippet), not speech. Check its time: if it is more than a few minutes old, tell me, in case it is not the one I mean. {task}",
        done: None,
    },
    PromptDef {
        id: "find-topic",
        title: "Find where I talked about something",
        description: "Searches everything you said for a topic and answers with dates and short quotes.",
        kinds: SPOKEN,
        args: &[TOPIC],
        text: "Find where I talked about {topic}. Call search_items with a few of the words I would have said, then read the best matches in full with get_items. Names and unusual words are often misheard, so if little comes back, search again with other spellings or related words. Answer with the dates, what I said each time in a sentence, and how my thinking changed, if it did. {speech}",
        done: None,
    },
    PromptDef {
        id: "weekly-review",
        title: "Weekly review",
        description: "What you said this week: the themes, the decisions and the loose ends.",
        kinds: SPOKEN,
        args: &[],
        text: "Do my weekly review. Call library_stats with when \"last 7 days\" and group_by day to see the busy days, then list_items with when \"last 7 days\" and kinds {kinds} (follow next_cursor), reading in full with get_items whatever is cut and matters. Give me: the main themes of my week, decisions I made, things I said I would do and have not mentioned since, and three things to carry into next week. Keep it to one screen. {speech}",
        done: None,
    },
    PromptDef {
        id: "draft-from-dictations",
        title: "Draft from my dictations",
        description: "Turns your rambling dictations on a topic into one clean draft in your voice.",
        kinds: &["dictation", "note"],
        args: &[
            TOPIC,
            Arg { name: "day", description: "When I said it: today, yesterday, this week, Wednesday. Default today.", required: false, default: Some("today"), placeholder: "[day]" },
        ],
        text: "Write a clean draft about {topic} from what I said {day}. Use list_items with when \"{day}\" and kinds {kinds}, and search_items for the topic with the same when, then read the matching items in full with get_items, in the order I said them. Keep my points, my order and my way of putting things. Drop false starts, repeats and filler. Where a word looks misheard and you cannot tell what I meant, mark it [?] rather than guess. Show me the draft and the ids you used. {speech}",
        done: None,
    },
    PromptDef {
        id: "how-i-write",
        title: "Teach you how I write",
        description: "Derives a style guide from the messages you dictated, for the agent to remember.",
        kinds: &["dictation"],
        args: &[],
        text: "Learn how I write from my dictations. Call list_items with kinds [\"dictation\"] and limit 200, and read a good spread of them (get_items for the longer ones). Most are messages I typed by voice into other apps. Write a short style guide: tone, length, how I open and close, punctuation and capitals, words and phrases I use often, what changes with who I am writing to. Ignore recognition mistakes; they are not my style. Give examples as short made-up lines, not my real messages. Then save the guide wherever you keep instructions about me, or tell me where to put it. {speech}",
        done: None,
    },
    PromptDef {
        id: "open-loops",
        title: "What did I say I would do",
        description: "Finds the promises and to-dos in two weeks of what you said, and which are still open.",
        kinds: SPOKEN,
        args: &[],
        text: "Find the things I said I would do in the last two weeks. Call list_items with when \"last 14 days\" and kinds {kinds} (follow next_cursor) and search_items with the same when for phrases like \"I'll\", \"I will\", \"remind me\", \"need to\" and \"follow up\". For each, give the date, what I committed to and to whom, and whether anything later suggests it is done. Put the open ones first. {speech}",
        done: None,
    },
];

pub fn find(id: &str) -> Option<&'static PromptDef> {
    let wanted = id.trim().trim_start_matches('/').to_ascii_lowercase();
    PROMPTS.iter().find(|p| p.id == wanted)
}

/// The prompts a server with this view offers.
pub fn offered(scope: &Scope) -> Vec<&'static PromptDef> {
    PROMPTS.iter().filter(|p| p.offered(scope)).collect()
}

/// A prompt's text, for a server with `access`. Arguments that are given replace their
/// `{name}`. One left out takes its default, or with `fill` its placeholder (`[topic]`) for a
/// person to fill in; without `fill` a missing required argument is an error.
pub fn render(def: &PromptDef, given: &BTreeMap<String, String>, access: &Access, fill: bool) -> Result<String> {
    if !def.offered(&access.scope) {
        bail!(
            "\"{}\" needs {}, which this connection does not show ({})",
            def.id,
            def.kinds.join(" or "),
            access.scope.short().unwrap_or_default()
        );
    }
    if let Some(unknown) = given.keys().find(|name| !def.args.iter().any(|a| a.name == name.as_str())) {
        let known: Vec<&str> = def.args.iter().map(|a| a.name).collect();
        if known.is_empty() {
            bail!("\"{}\" takes no arguments, not \"{unknown}\"", def.id);
        }
        bail!("\"{}\" takes {}, not \"{unknown}\"", def.id, known.join(", "));
    }
    let mut text = def.text.to_string();
    for arg in def.args {
        let value = match given.get(arg.name).map(|v| v.trim()).filter(|v| !v.is_empty()) {
            Some(value) => value.to_string(),
            None => match arg.default {
                Some(default) => default.to_string(),
                None if fill => arg.placeholder.to_string(),
                None => bail!("\"{}\" needs {}: {}", def.id, arg.name, arg.description),
            },
        };
        text = text.replace(&format!("{{{}}}", arg.name), &value);
    }
    let kinds: Vec<&str> = def.kinds.iter().copied().filter(|k| access.scope.allows_kind(k)).collect();
    text = text.replace("{kinds}", &json!(kinds).to_string());
    if let Some((write, read_only)) = def.done {
        text = text.replace("{done}", if access.read_only { read_only } else { write });
    }
    Ok(text.replace("{speech}", SPEECH).trim().to_string())
}

/// One prompt as `zone prompts --json` and the page show it: its text with defaults and
/// placeholders filled in.
pub fn to_json(def: &PromptDef, access: &Access) -> Value {
    let args: Vec<Value> = def
        .args
        .iter()
        .map(|a| json!({ "name": a.name, "description": a.description, "required": a.required, "default": a.default }))
        .collect();
    json!({
        "id": def.id,
        "title": def.title,
        "description": def.description,
        "kinds": def.kinds,
        "writes": def.writes(),
        "arguments": args,
        "text": render(def, &BTreeMap::new(), access, true).unwrap_or_default(),
    })
}

/// Every prompt a server with `access` offers, as JSON.
pub fn list_json(access: &Access) -> Value {
    let list: Vec<Value> = offered(&access.scope).into_iter().map(|def| to_json(def, access)).collect();
    json!({ "prompts": list })
}

/// `name=value` pairs from the command line.
pub fn parse_pairs(pairs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for pair in pairs {
        match pair.split_once('=') {
            Some((name, value)) if !name.trim().is_empty() => {
                out.insert(name.trim().to_ascii_lowercase(), value.to_string());
            }
            _ => bail!("arguments are name=value, for example day=Wednesday or topic=\"the lease\", not \"{pair}\""),
        }
    }
    Ok(out)
}

/// `zone prompts [--json]`
pub fn list(access: &Access, as_json: bool) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(&list_json(access))?);
        return Ok(());
    }
    let shown = offered(&access.scope);
    let width = shown.iter().map(|p| p.id.len()).max().unwrap_or(10);
    for def in &shown {
        println!("{:width$}  {}", def.id, def.title);
        println!("{:width$}  {}", "", def.description);
    }
    println!();
    println!("`zone prompts show <id>` prints one to copy into any agent, for example: zone prompts show what-did-i-say day=Wednesday");
    println!("Agents that support MCP prompts also offer these as slash commands.");
    Ok(())
}

/// `zone prompts show <id> [name=value ...] [--json]`
pub fn show(id: &str, pairs: &[String], access: &Access, as_json: bool) -> Result<()> {
    let Some(def) = find(id) else {
        let ids: Vec<&str> = PROMPTS.iter().map(|p| p.id).collect();
        bail!("no prompt is called \"{id}\". The prompts: {}", ids.join(", "));
    };
    let given = parse_pairs(pairs)?;
    let text = render(def, &given, access, true)?;
    if as_json {
        let mut value = to_json(def, access);
        value["text"] = json!(text);
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("{text}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::definitions;

    fn access(kinds: &[&str], read_only: bool) -> Access {
        let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
        Access { read_only, scope: Scope::new(&kinds, &[]).unwrap() }
    }

    #[test]
    fn every_prompt_is_well_formed() {
        let tools: Vec<&str> = definitions(&Access::default()).iter().map(|t| t.name).collect();
        let mut ids = std::collections::HashSet::new();
        assert!((8..=12).contains(&PROMPTS.len()));
        for def in PROMPTS {
            assert!(ids.insert(def.id), "{} twice", def.id);
            assert!(def.id.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "{}", def.id);
            for kind in def.kinds {
                assert!(crate::scope::KINDS.contains(kind), "{}: {kind}", def.id);
            }
            let text = render(def, &BTreeMap::new(), &Access::default(), true).unwrap();
            // Every placeholder is filled, and the text names at least one real tool.
            assert!(!text.contains('{') && !text.contains('}'), "{}: {text}", def.id);
            assert!(tools.iter().any(|tool| text.contains(tool)), "{} names no tool", def.id);
            for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| w.contains('_') && w.ends_with("_items")) {
                assert!(tools.contains(&word), "{} names {word}, which is not a tool", def.id);
            }
            assert!(!text.contains('\u{2014}') && !def.description.contains('\u{2014}') && !def.title.contains('\u{2014}'), "{}", def.id);
            // Speech prompts say it is speech. The clip prompt says it is not.
            assert_eq!(text.contains("misheard"), def.kinds != ["clip"], "{}", def.id);
            assert!(def.description.len() <= 110, "{} has a long description", def.id);
        }
    }

    #[test]
    fn arguments_defaults_and_placeholders() {
        let day = find("what-did-i-say").unwrap();
        let everything = Access::default();
        let text = render(day, &BTreeMap::new(), &everything, false).unwrap();
        assert!(text.starts_with("What did I say today?"), "{text}");
        let given = parse_pairs(&["day=Wednesday 12pm to 5pm".to_string()]).unwrap();
        assert!(render(day, &given, &everything, false).unwrap().starts_with("What did I say Wednesday 12pm to 5pm?"));
        assert!(render(day, &given, &everything, false).unwrap().contains(r#"kinds ["dictation","note","meeting"]"#));

        let find_topic = find("find-topic").unwrap();
        // Over MCP a required argument must be given. For a person, a placeholder shows.
        let missing = render(find_topic, &BTreeMap::new(), &everything, false).unwrap_err().to_string();
        assert!(missing.contains("needs topic"), "{missing}");
        assert!(render(find_topic, &BTreeMap::new(), &everything, true).unwrap().starts_with("Find where I talked about [topic]."));
        let wrong = render(find_topic, &parse_pairs(&["subject=x".to_string()]).unwrap(), &everything, true).unwrap_err().to_string();
        assert!(wrong.contains("takes topic"), "{wrong}");
        assert!(parse_pairs(&["nonsense".to_string()]).is_err());
        assert!(find("/Find-Topic").is_some() && find("nope").is_none());
    }

    #[test]
    fn a_limited_or_read_only_view_gets_prompts_that_fit_it() {
        let notes = access(&["note"], false);
        let ids: Vec<&str> = offered(&notes.scope).iter().map(|p| p.id).collect();
        assert!(ids.contains(&"notes-to-plan") && ids.contains(&"what-did-i-say") && !ids.contains(&"just-sent") && !ids.contains(&"last-meeting") && !ids.contains(&"how-i-write"), "{ids:?}");
        // The kinds a prompt names are only those this view shows.
        let text = render(find("weekly-review").unwrap(), &BTreeMap::new(), &notes, true).unwrap();
        assert!(text.contains(r#"kinds ["note"]"#), "{text}");
        assert!(render(find("just-sent").unwrap(), &BTreeMap::new(), &notes, true).unwrap_err().to_string().contains("does not show"));
        // Only clips: only the clip prompt.
        let clips = access(&["clip"], false);
        assert_eq!(offered(&clips.scope).iter().map(|p| p.id).collect::<Vec<_>>(), ["just-sent"]);
        // Read-only: the prompts that mark items done say they cannot.
        let plan = find("notes-to-plan").unwrap();
        assert!(render(plan, &BTreeMap::new(), &Access::default(), true).unwrap().contains("call mark_processed"));
        let read_only = render(plan, &BTreeMap::new(), &access(&[], true), true).unwrap();
        assert!(!read_only.contains("mark_processed") && read_only.contains("read-only"), "{read_only}");
        let listed = list_json(&notes);
        assert!(listed["prompts"].as_array().unwrap().iter().all(|p| p["text"].as_str().unwrap().len() > 100));
    }
}
