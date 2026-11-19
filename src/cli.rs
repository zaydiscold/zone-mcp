//! The query commands for people and scripts: list, search, show, stats, export.

use std::io::Write;

use anyhow::{Result, bail};
use clap::Args;
use serde_json::{Map, Value, json};

use crate::db::{Db, Filter, Order, Row, SearchCursor};
use crate::item::preview;
use crate::timeparse::{Bound, Clock, human_duration};
use crate::tools::normalize_kind;

#[derive(Args, Clone, Debug, Default)]
pub struct FilterArgs {
    /// A window in words, instead of --since and --until: "Wednesday 12pm to 5pm",
    /// "yesterday afternoon", "last week", "3 days ago"
    #[arg(long, value_name = "WORDS", conflicts_with_all = ["since", "until"])]
    pub when: Option<String>,
    /// Only items created at or after this time: 2026-11-11, "2026-11-11 14:30",
    /// 2026-11-11T14:30:00-08:00, 2h, 3d, today, yesterday
    #[arg(long, value_name = "TIME")]
    pub since: Option<String>,
    /// Only items created before this time (a bare date means through the end of that day)
    #[arg(long, value_name = "TIME")]
    pub until: Option<String>,
    /// Only this kind: dictation, note, meeting or clip. Repeat for more than one
    #[arg(long, value_name = "KIND")]
    pub kind: Vec<String>,
    /// Only items from here: keyboard, app or note (dictations), clipboard, share, shortcut
    /// or typed (clips). Repeat for more than one
    #[arg(long, value_name = "SOURCE")]
    pub source: Vec<String>,
    /// Only items not flagged as processed (on the page: not marked Done)
    #[arg(long)]
    pub unprocessed: bool,
    /// Only items flagged as processed (on the page: marked Done)
    #[arg(long, conflicts_with = "unprocessed")]
    pub done: bool,
    /// Only items from this phone: its name, or the beginning of its id from `zone devices`
    #[arg(long, value_name = "PHONE")]
    pub phone: Option<String>,
    /// Read and show times in this time zone (for example America/Los_Angeles) instead of
    /// the computer's
    #[arg(long, value_name = "ZONE")]
    pub tz: Option<String>,
}

impl FilterArgs {
    pub fn clock(&self) -> Result<Clock> {
        Clock::in_zone(self.tz.as_deref())
    }

    pub fn filter(&self, clock: &Clock) -> Result<Filter> {
        let (start, end) = match &self.when {
            Some(words) => {
                let window = clock.window(words)?;
                (Some(window.start), Some(window.end))
            }
            None => (
                self.since.as_deref().map(|t| clock.parse(t, Bound::Start)).transpose()?,
                self.until.as_deref().map(|t| clock.parse(t, Bound::End)).transpose()?,
            ),
        };
        if let (Some(s), Some(e)) = (start, end)
            && s >= e {
                bail!("--since ({}) is not before --until ({})", clock.full(s), clock.full(e));
            }
        let split = |values: &[String]| -> Vec<String> {
            values.iter().flat_map(|v| v.split(',')).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string).collect()
        };
        Ok(Filter {
            start,
            end,
            kinds: split(&self.kind).iter().map(|k| normalize_kind(k)).collect(),
            sources: split(&self.source),
            unprocessed_only: self.unprocessed,
            processed_only: self.done,
            ..Filter::default()
        })
    }

    /// The filter, with `--phone` looked up among the paired phones.
    pub fn filter_in(&self, db: &Db, clock: &Clock) -> Result<Filter> {
        let mut filter = self.filter(clock)?;
        if let Some(spec) = &self.phone {
            let device = db.find_device(spec)?;
            let id = device.phone_id.clone().ok_or_else(|| anyhow::anyhow!("{} never said which phone it is, so its items cannot be told apart", device.label()))?;
            filter.phone = Some(crate::db::FromPhone::Id(id));
        }
        Ok(filter)
    }
}

/// An item as JSON, for `--json` and for export: the wire fields plus what the computer knows.
pub fn item_json(row: &Row, clock: &Clock) -> Value {
    let extra: Value = serde_json::from_str(&row.extra).unwrap_or_else(|_| json!({}));
    json!({
        "id": row.id,
        "kind": row.kind,
        "source": row.source,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
        "created_local": clock.rfc3339(row.created_at),
        "text": row.text,
        "title": row.title,
        "duration_s": row.duration_s,
        "words": row.words,
        "engine": row.engine,
        "status": row.status,
        "extra": extra,
        "rev": row.rev,
        "processed_at": row.processed_at,
        "carrier": row.carrier,
        "device_id": row.device_id,
        "received_at": row.received_at,
    })
}

fn label(row: &Row) -> String {
    match &row.source {
        Some(source) => format!("{}/{}", row.kind, source),
        None => row.kind.clone(),
    }
}

/// One item as two lines: what and when, then the beginning of the text.
fn print_card(out: &mut impl Write, row: &Row, clock: &Clock, body: &str) -> std::io::Result<()> {
    let mut head = format!("{}  {}", clock.short(row.created_at), label(row));
    if let Some(seconds) = row.duration_s {
        head.push_str(&format!("  {}", human_duration(seconds)));
    }
    head.push_str(&format!("  {} {}", row.words, if row.words == 1 { "word" } else { "words" }));
    if row.processed_at.is_some() {
        head.push_str("  processed");
    }
    head.push_str(&format!("  {}", &row.id[..row.id.len().min(8)]));
    writeln!(out, "{head}")?;
    if let Some(title) = &row.title {
        writeln!(out, "    {title}")?;
    }
    if !body.is_empty() {
        writeln!(out, "    {body}")?;
    }
    Ok(())
}

pub fn list(db: &Db, args: &FilterArgs, order: &str, limit: usize, all: bool, as_json: bool) -> Result<()> {
    let clock = args.clock()?;
    let filter = args.filter_in(db, &clock)?;
    let order = Order::parse(order)?;
    let total = db.count(&filter)?;
    let limit = if all { usize::MAX / 2 } else { limit.max(1) };
    let rows = db.list(&filter, order, limit, None)?;
    let mut out = std::io::stdout().lock();
    if as_json {
        let items: Vec<Value> = rows.iter().map(|row| item_json(row, &clock)).collect();
        writeln!(out, "{}", serde_json::to_string_pretty(&json!({ "total": total, "items": items }))?)?;
        return Ok(());
    }
    for row in &rows {
        print_card(&mut out, row, &clock, &preview(&row.text, 200))?;
    }
    if rows.is_empty() {
        writeln!(out, "No items{}.", if total == 0 && db.total_items()? == 0 { " yet. Pair a phone with `zone pair`" } else { " match" })?;
    } else if (rows.len() as i64) < total {
        writeln!(out, "\nShowing {} of {total}. Use --limit N or --all for more.", rows.len())?;
    } else {
        writeln!(out, "\n{total} {}.", if total == 1 { "item" } else { "items" })?;
    }
    Ok(())
}

pub fn search(db: &Db, query: &str, args: &FilterArgs, order: Option<&str>, limit: usize, cursor: Option<&str>, as_json: bool) -> Result<()> {
    let clock = args.clock()?;
    let filter = args.filter_in(db, &clock)?;
    let order = order.map(Order::parse).transpose()?;
    let offset = cursor.map(SearchCursor::decode).transpose()?.map(|c| c.0).unwrap_or(0);
    let result = db.search_page(query, &filter, order, limit.max(1), offset)?;
    let next_cursor = SearchCursor::next(offset, result.hits.len(), result.total).map(|c| c.encode());
    let mut out = std::io::stdout().lock();
    if as_json {
        let items: Vec<Value> = result
            .hits
            .iter()
            .map(|hit| {
                let mut value = item_json(&hit.row, &clock);
                value["snippet"] = json!(hit.snippet);
                value
            })
            .collect();
        writeln!(out, "{}", serde_json::to_string_pretty(&json!({ "total": result.total, "matched": result.matched, "next_cursor": next_cursor, "items": items }))?)?;
        return Ok(());
    }
    for hit in &result.hits {
        print_card(&mut out, &hit.row, &clock, &hit.snippet.replace('\n', " "))?;
    }
    if result.hits.is_empty() && offset == 0 {
        writeln!(out, "Nothing matches \"{query}\".")?;
    } else if let Some(next) = &next_cursor {
        writeln!(
            out,
            "\nShowing matches {} to {} of {}. For the next ones, run the same search with --cursor {next}",
            offset + 1,
            offset + result.hits.len(),
            result.total
        )?;
    } else if offset > 0 {
        writeln!(out, "\nThat is the last of {} matches.", result.total)?;
    } else {
        writeln!(out, "\n{} {}.", result.total, if result.total == 1 { "match" } else { "matches" })?;
    }
    if result.matched == "prefix" {
        writeln!(out, "No whole word matched, so words were matched by their beginnings.")?;
    }
    Ok(())
}

pub fn show(db: &Db, ids: &[String], tz: Option<&str>, as_json: bool) -> Result<()> {
    let clock = Clock::in_zone(tz)?;
    let mut out = std::io::stdout().lock();
    let mut rows = Vec::new();
    for id in ids {
        rows.push(db.resolve(id)?);
    }
    if as_json {
        let items: Vec<Value> = rows.iter().map(|row| item_json(row, &clock)).collect();
        writeln!(out, "{}", serde_json::to_string_pretty(&items)?)?;
        return Ok(());
    }
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        writeln!(out, "{}  {}", clock.full(row.created_at), label(row))?;
        writeln!(out, "id        {}", row.id)?;
        if let Some(title) = &row.title {
            writeln!(out, "title     {title}")?;
        }
        if row.updated_at > row.created_at + 1.0 {
            writeln!(out, "changed   {}", clock.full(row.updated_at))?;
        }
        if let Some(seconds) = row.duration_s {
            writeln!(out, "recorded  {}", human_duration(seconds))?;
        }
        writeln!(out, "words     {}", row.words)?;
        let details: Vec<String> = [row.engine.as_ref().map(|e| format!("engine {e}")), row.status.as_ref().map(|s| format!("status {s}"))]
            .into_iter()
            .flatten()
            .collect();
        if !details.is_empty() {
            writeln!(out, "phone     {}", details.join(", "))?;
        }
        writeln!(out, "arrived   {} by {}", clock.full(row.received_at), row.carrier)?;
        if let Some(processed) = row.processed_at {
            writeln!(out, "processed {}", clock.full(processed))?;
        }
        if row.deleted {
            writeln!(out, "\n(deleted on the phone: the text is gone)")?;
        } else {
            writeln!(out, "\n{}", row.text)?;
        }
    }
    Ok(())
}

pub fn stats(db: &Db, args: &FilterArgs, by: &str, as_json: bool) -> Result<()> {
    let clock = args.clock()?;
    let filter = args.filter_in(db, &clock)?;
    if !matches!(by, "day" | "kind" | "source") {
        bail!("--by must be day, kind or source");
    }
    let rows = db.stat_rows(&filter)?;
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, (i64, i64, f64)> = std::collections::HashMap::new();
    let (mut items, mut words, mut seconds) = (0i64, 0i64, 0.0f64);
    for row in &rows {
        let key = match by {
            "day" => clock.zoned(row.created_at).strftime("%a %Y-%m-%d").to_string(),
            "kind" => row.kind.clone(),
            _ => row.source.clone().unwrap_or_else(|| "(none)".to_string()),
        };
        let group = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            (0, 0, 0.0)
        });
        group.0 += 1;
        group.1 += row.words;
        group.2 += row.duration_s.unwrap_or(0.0);
        items += 1;
        words += row.words;
        seconds += row.duration_s.unwrap_or(0.0);
    }
    if by != "day" {
        order.sort_by(|a, b| groups[b].0.cmp(&groups[a].0).then(a.cmp(b)));
    }
    let mut out = std::io::stdout().lock();
    if as_json {
        let list: Vec<Value> = order
            .iter()
            .map(|key| {
                let (n, w, s) = groups[key];
                let mut entry = Map::new();
                entry.insert(by.to_string(), json!(key));
                entry.insert("items".into(), json!(n));
                entry.insert("words".into(), json!(w));
                entry.insert("recorded_s".into(), json!(s.round()));
                Value::Object(entry)
            })
            .collect();
        let value = json!({ "total": { "items": items, "words": words, "recorded_s": seconds.round() }, "groups": list });
        writeln!(out, "{}", serde_json::to_string_pretty(&value)?)?;
        return Ok(());
    }
    let width = order.iter().map(|k| k.chars().count()).max().unwrap_or(4).max(by.len());
    writeln!(out, "{:width$}  {:>7}  {:>9}  {:>9}", by, "items", "words", "recorded")?;
    for key in &order {
        let (n, w, s) = groups[key];
        writeln!(out, "{key:width$}  {n:>7}  {w:>9}  {:>9}", human_duration(s))?;
    }
    writeln!(out, "{:width$}  {items:>7}  {words:>9}  {:>9}", "total", human_duration(seconds))?;
    Ok(())
}

pub fn export(db: &Db, args: &FilterArgs, format: &str, order: &str, ids: &[String], output: Option<&std::path::Path>) -> Result<()> {
    let clock = args.clock()?;
    let format = crate::export::Format::parse(format)?;
    let order = Order::parse(order)?;
    let rows = if ids.is_empty() {
        db.list(&args.filter_in(db, &clock)?, Order::Oldest, usize::MAX / 2, None)?
    } else {
        let mut rows = Vec::new();
        for id in ids {
            let row = db.resolve(id)?;
            if row.deleted {
                bail!("{} is not in the library any more", row.id);
            }
            rows.push(row);
        }
        rows
    };
    let text = crate::export::render(&rows, &clock, format, order, None);
    match output {
        Some(path) => {
            std::fs::write(path, text)?;
            eprintln!("Wrote {} items to {}", rows.len(), path.display());
        }
        None => {
            let mut out = std::io::stdout().lock();
            out.write_all(text.as_bytes())?;
            out.flush()?;
        }
    }
    Ok(())
}
