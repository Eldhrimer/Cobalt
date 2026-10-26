//! A section as the reader holds it: its blocks, the designer's queued
//! operations, and the working copy that replaying those operations produces.
//!
//! The queue is the source of truth. The working copy is rebuilt from the
//! section as the gateway sent it plus every queued operation, so undoing is
//! removing the last entry and a restart loses nothing.
//!
//! Everything here mirrors the gateway's `drafter/ops.py` rule for rule,
//! because the reader and the gateway each apply the same operations and must
//! land on the same text. `tests/golden.json` holds vectors both sides run.

use kobo_json::{ObjectBuilder, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Heading,
    Para,
    Callout,
    Quote,
    List,
    Table,
    Code,
    Hr,
    Marker,
}

impl Kind {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "heading" => Self::Heading,
            "para" => Self::Para,
            "callout" => Self::Callout,
            "quote" => Self::Quote,
            "list" => Self::List,
            "table" => Self::Table,
            "code" => Self::Code,
            "hr" => Self::Hr,
            "marker" => Self::Marker,
            _ => return None,
        })
    }

    pub const fn noun(self) -> &'static str {
        match self {
            Self::Heading => "Heading",
            Self::Para => "Paragraph",
            Self::Callout => "Callout",
            Self::Quote => "Quote",
            Self::List => "List",
            Self::Table => "Table",
            Self::Code => "Code",
            Self::Hr => "Rule",
            Self::Marker => "Marker",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Style {
    Bold,
    Italic,
    BoldItalic,
    Code,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Part {
    pub text: String,
    pub spans: Vec<(usize, usize, Style)>,
    pub depth: u8,
    pub ordered: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Row {
    pub header: bool,
    pub cells: Vec<String>,
}

/// What the gateway says happened to a block in a proposal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Change {
    #[default]
    Same,
    Yours,
    Revised,
    Added,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    pub id: String,
    pub kind: Kind,
    pub level: u8,
    pub label: String,
    pub title: String,
    pub parts: Vec<Part>,
    pub rows: Vec<Row>,
    pub change: Change,
    pub before: Option<Box<Block>>,
    pub why: String,
}

impl Block {
    pub fn text(id: &str, kind: Kind, text: &str) -> Self {
        Self {
            id: id.to_owned(),
            kind,
            level: 0,
            label: String::new(),
            title: String::new(),
            parts: vec![Part {
                text: text.to_owned(),
                ..Part::default()
            }],
            rows: Vec::new(),
            change: Change::Same,
            before: None,
            why: String::new(),
        }
    }

    /// One line a reader can recognise the block by.
    pub fn snippet(&self, limit: usize) -> String {
        let source = match self.kind {
            Kind::Table => self
                .rows
                .first()
                .map(|row| row.cells.join(" | "))
                .unwrap_or_default(),
            Kind::Hr => "Rule".to_owned(),
            _ => self
                .parts
                .first()
                .map(|p| p.text.clone())
                .unwrap_or_default(),
        };
        let flat = source.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.chars().count() <= limit {
            return flat;
        }
        let cut: String = flat.chars().take(limit).collect();
        format!("{}...", cut.trim_end())
    }
}

// ------------------------------------------------------------------ decode

fn style_of(name: &str) -> Option<Style> {
    Some(match name {
        "b" => Style::Bold,
        "i" => Style::Italic,
        "bi" => Style::BoldItalic,
        "code" => Style::Code,
        _ => return None,
    })
}

fn text_at(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn number(value: &Value) -> Option<usize> {
    usize::try_from(value.as_i64()?).ok()
}

fn part_from(value: &Value) -> Option<Part> {
    let text = value.get("text")?.as_str()?.to_owned();
    let mut spans = Vec::new();
    for span in value
        .get("spans")
        .and_then(Value::as_array)
        .unwrap_or_default()
    {
        let (start, end) = (number(span.index(0)?)?, number(span.index(1)?)?);
        let style = style_of(span.index(2)?.as_str()?)?;
        if start < end
            && end <= text.len()
            && text.is_char_boundary(start)
            && text.is_char_boundary(end)
        {
            spans.push((start, end, style));
        }
    }
    Some(Part {
        text,
        spans,
        depth: value
            .get("depth")
            .and_then(Value::as_i64)
            .map_or(0, |d| u8::try_from(d.clamp(0, 6)).unwrap_or(0)),
        ordered: value
            .get("ordered")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

pub fn block_from(value: &Value) -> Option<Block> {
    let kind = Kind::parse(value.get("kind")?.as_str()?)?;
    let mut parts = Vec::new();
    if let Some(list) = value.get("parts").and_then(Value::as_array) {
        for part in list {
            parts.push(part_from(part)?);
        }
    } else if value.get("text").is_some() {
        parts.push(part_from(value)?);
    }
    let mut rows = Vec::new();
    for row in value
        .get("rows")
        .and_then(Value::as_array)
        .unwrap_or_default()
    {
        let cells = row
            .get("cells")?
            .as_array()?
            .iter()
            .map(|c| c.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        rows.push(Row {
            header: row.get("header").and_then(Value::as_bool).unwrap_or(false),
            cells,
        });
    }
    let change = match value.get("change").and_then(Value::as_str) {
        Some("yours") => Change::Yours,
        Some("revised") => Change::Revised,
        Some("added") => Change::Added,
        _ => Change::Same,
    };
    Some(Block {
        id: value.get("id")?.as_str()?.to_owned(),
        kind,
        level: value
            .get("level")
            .and_then(Value::as_i64)
            .map_or(0, |l| u8::try_from(l.clamp(0, 6)).unwrap_or(0)),
        label: text_at(value, "label"),
        title: text_at(value, "title"),
        parts,
        rows,
        change,
        before: value.get("before").and_then(block_from).map(Box::new),
        why: text_at(value, "why"),
    })
}

pub fn blocks_from(value: &Value) -> Option<Vec<Block>> {
    value.as_array()?.iter().map(block_from).collect()
}

// --------------------------------------------------------------------- ops

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ask {
    Tighten,
    Plainer,
    Concrete,
    Cut,
    Keep,
    Check,
    Custom,
}

impl Ask {
    pub const ALL: [Self; 7] = [
        Self::Tighten,
        Self::Plainer,
        Self::Concrete,
        Self::Cut,
        Self::Keep,
        Self::Check,
        Self::Custom,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Tighten => "tighten",
            Self::Plainer => "plainer",
            Self::Concrete => "concrete",
            Self::Cut => "cut",
            Self::Keep => "keep",
            Self::Check => "check",
            Self::Custom => "custom",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Tighten => "Tighten",
            Self::Plainer => "Say it plainer",
            Self::Concrete => "Make it concrete",
            Self::Cut => "Cut it",
            Self::Keep => "Keep as written",
            Self::Check => "Check against the rules",
            Self::Custom => "Write a note",
        }
    }

    pub const fn detail(self) -> &'static str {
        match self {
            Self::Tighten => "Same content, fewer words.",
            Self::Plainer => "Simpler words and structure.",
            Self::Concrete => "Show what someone does.",
            Self::Cut => "Remove it and mend the join.",
            Self::Keep => "Protect it from other changes.",
            Self::Check => "Flag problems; change nothing.",
            Self::Custom => "Tell the reviser what you want.",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|ask| ask.name() == name)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Op {
    Mark {
        block: String,
        part: usize,
        start: usize,
        end: usize,
        text: String,
        ask: Ask,
        note: String,
    },
    Note {
        block: String,
        note: String,
    },
    Want {
        after: Option<String>,
        note: String,
    },
    Move {
        block: String,
        after: Option<String>,
    },
    Split {
        block: String,
        at: usize,
        new: String,
    },
    Merge {
        block: String,
    },
    Delete {
        block: String,
    },
    Edit {
        block: String,
        text: String,
    },
    Insert {
        after: Option<String>,
        text: String,
        new: String,
    },
}

fn opt(value: Option<&String>) -> Value {
    value.map_or(Value::Null, |s| Value::String(s.clone()))
}

fn size(n: usize) -> Value {
    Value::Number(f64::from(u32::try_from(n).unwrap_or(u32::MAX)))
}

impl Op {
    pub fn to_value(&self) -> Value {
        let base = |name: &str| ObjectBuilder::new().set("op", name);
        match self {
            Self::Mark {
                block,
                part,
                start,
                end,
                text,
                ask,
                note,
            } => base("mark")
                .set("block", block.as_str())
                .set("part", size(*part))
                .set("start", size(*start))
                .set("end", size(*end))
                .set("text", text.as_str())
                .set("ask", ask.name())
                .set("note", note.as_str()),
            Self::Note { block, note } => base("note")
                .set("block", block.as_str())
                .set("note", note.as_str()),
            Self::Want { after, note } => base("want")
                .set("after", opt(after.as_ref()))
                .set("note", note.as_str()),
            Self::Move { block, after } => base("move")
                .set("block", block.as_str())
                .set("after", opt(after.as_ref())),
            Self::Split { block, at, new } => base("split")
                .set("block", block.as_str())
                .set("at", size(*at))
                .set("new", new.as_str()),
            Self::Merge { block } => base("merge").set("block", block.as_str()),
            Self::Delete { block } => base("delete").set("block", block.as_str()),
            Self::Edit { block, text } => base("edit")
                .set("block", block.as_str())
                .set("text", text.as_str()),
            Self::Insert { after, text, new } => base("insert")
                .set("after", opt(after.as_ref()))
                .set("text", text.as_str())
                .set("new", new.as_str()),
        }
        .build()
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        let after = || match value.get("after")? {
            Value::Null => Some(None),
            Value::String(s) => Some(Some(s.clone())),
            _ => None,
        };
        Some(match value.get("op")?.as_str()? {
            "mark" => Self::Mark {
                block: text("block")?,
                part: value.get("part").and_then(number).unwrap_or(0),
                start: number(value.get("start")?)?,
                end: number(value.get("end")?)?,
                text: text("text")?,
                ask: Ask::parse(value.get("ask")?.as_str()?)?,
                note: text("note").unwrap_or_default(),
            },
            "note" => Self::Note {
                block: text("block")?,
                note: text("note")?,
            },
            "want" => Self::Want {
                after: after()?,
                note: text("note")?,
            },
            "move" => Self::Move {
                block: text("block")?,
                after: after()?,
            },
            "split" => Self::Split {
                block: text("block")?,
                at: number(value.get("at")?)?,
                new: text("new")?,
            },
            "merge" => Self::Merge {
                block: text("block")?,
            },
            "delete" => Self::Delete {
                block: text("block")?,
            },
            "edit" => Self::Edit {
                block: text("block")?,
                text: text("text")?,
            },
            "insert" => Self::Insert {
                after: after()?,
                text: text("text")?,
                new: text("new")?,
            },
            _ => return None,
        })
    }

    /// One line for the queue screen.
    pub fn describe(&self) -> String {
        match self {
            Self::Mark { ask, text, .. } => format!("{}: {}", ask.label(), clip(text, 60)),
            Self::Note { note, .. } => format!("Note: {}", clip(note, 60)),
            Self::Want { note, .. } => format!("New paragraph wanted: {}", clip(note, 50)),
            Self::Move { .. } => "Moved a block".to_owned(),
            Self::Split { .. } => "Split a paragraph".to_owned(),
            Self::Merge { .. } => "Merged two paragraphs".to_owned(),
            Self::Delete { .. } => "Deleted a block".to_owned(),
            Self::Edit { text, .. } => format!("Retyped: {}", clip(text, 60)),
            Self::Insert { text, .. } => format!("Added: {}", clip(text, 60)),
        }
    }

    /// Whether the gateway will ask the model about this operation.
    pub const fn is_instruction(&self) -> bool {
        matches!(
            self,
            Self::Mark { .. } | Self::Note { .. } | Self::Want { .. }
        )
    }
}

pub fn clip(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        flat
    } else {
        let cut: String = flat.chars().take(limit).collect();
        format!("{}...", cut.trim_end())
    }
}

pub fn ops_to_json(ops: &[Op]) -> String {
    Value::Array(ops.iter().map(Op::to_value).collect()).to_json()
}

pub fn ops_from_json(text: &str) -> Option<Vec<Op>> {
    let value = kobo_json::parse(text).ok()?;
    value.as_array()?.iter().map(Op::from_value).collect()
}

// ----------------------------------------------------------------- working

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mark {
    pub block: String,
    pub excerpt: String,
    pub ask: Ask,
}

#[derive(Clone, Debug, Default)]
pub struct Working {
    pub blocks: Vec<Block>,
    pub marks: Vec<Mark>,
    pub notes: usize,
    pub wants: usize,
}

impl Working {
    pub fn replay(base: &[Block], ops: &[Op]) -> Result<Self, String> {
        let mut work = Self {
            blocks: base.to_vec(),
            ..Self::default()
        };
        for op in ops {
            work.apply(op)?;
        }
        Ok(work)
    }

    fn index(&self, id: &str) -> Result<usize, String> {
        self.blocks
            .iter()
            .position(|b| b.id == id)
            .ok_or_else(|| format!("unknown block {id}"))
    }

    pub fn marks_in<'a>(&'a self, block: &'a str) -> impl Iterator<Item = &'a Mark> {
        self.marks.iter().filter(move |m| m.block == block)
    }

    /// A mark follows its words: if they left the block it was made on, it
    /// goes to the block that now holds them, and is dropped if none does.
    fn rehome(&mut self) {
        let blocks = &self.blocks;
        self.marks.retain_mut(|mark| {
            let holds = |b: &Block| b.parts.iter().any(|p| p.text.contains(&mark.excerpt));
            if blocks.iter().any(|b| b.id == mark.block && holds(b)) {
                return true;
            }
            if let Some(found) = blocks.iter().find(|b| holds(b)) {
                mark.block.clone_from(&found.id);
                return true;
            }
            false
        });
    }

    pub fn apply(&mut self, op: &Op) -> Result<(), String> {
        match op {
            Op::Move { block, after } => {
                let at = self.index(block)?;
                let moved = self.blocks.remove(at);
                let place = match after {
                    None => 0,
                    Some(target) if target == block => {
                        return Err("a block cannot follow itself".into())
                    }
                    Some(target) => self.index(target)? + 1,
                };
                self.blocks.insert(place, moved);
            }
            Op::Delete { block } => {
                let at = self.index(block)?;
                self.blocks.remove(at);
                self.marks.retain(|m| &m.block != block);
            }
            Op::Split { block, at, new } => self.split(block, *at, new)?,
            Op::Merge { block } => self.merge(block)?,
            Op::Edit { block, text } => {
                let at = self.index(block)?;
                let text = text.trim();
                if text.is_empty() || text.contains("\n\n") {
                    return Err("an edit is one non-empty paragraph".into());
                }
                let target = &mut self.blocks[at];
                if !matches!(target.kind, Kind::Para | Kind::Heading) {
                    return Err("only a paragraph or heading can be retyped".into());
                }
                target.parts = vec![Part {
                    text: text.replace('\n', " "),
                    ..Part::default()
                }];
                self.rehome();
            }
            Op::Insert { after, text, new } => {
                self.fresh(new)?;
                let place = match after {
                    None => 0,
                    Some(target) => self.index(target)? + 1,
                };
                self.blocks
                    .insert(place, Block::text(new, Kind::Para, text.trim()));
            }
            Op::Mark {
                block, text, ask, ..
            } => {
                let at = self.index(block)?;
                if text.trim().is_empty()
                    || !self.blocks[at].parts.iter().any(|p| p.text.contains(text))
                {
                    return Err("the highlighted text is not in that block".into());
                }
                self.marks.push(Mark {
                    block: block.clone(),
                    excerpt: text.clone(),
                    ask: *ask,
                });
            }
            Op::Note { block, .. } => {
                self.index(block)?;
                self.notes += 1;
            }
            Op::Want { after, .. } => {
                if let Some(target) = after {
                    self.index(target)?;
                }
                self.wants += 1;
            }
        }
        Ok(())
    }

    fn fresh(&self, id: &str) -> Result<(), String> {
        if self.blocks.iter().any(|b| b.id == id) {
            Err(format!("block id {id} is already used"))
        } else {
            Ok(())
        }
    }

    fn split(&mut self, block: &str, at: usize, new: &str) -> Result<(), String> {
        self.fresh(new)?;
        let place = self.index(block)?;
        let target = &self.blocks[place];
        if target.kind != Kind::Para || target.parts.len() != 1 {
            return Err("only a paragraph can be split".into());
        }
        let part = &target.parts[0];
        let text = &part.text;
        if at == 0 || at >= text.len() || !text.is_char_boundary(at) {
            return Err("the split point is not inside the paragraph".into());
        }
        let left = text[..at].trim_end();
        let tail = &text[at..];
        let skipped = tail.len() - tail.trim_start().len();
        let right_start = at + skipped;
        if left.is_empty() || right_start >= text.len() {
            return Err("a split needs text on both sides".into());
        }
        let clip = |from: usize, to: usize| -> Vec<(usize, usize, Style)> {
            part.spans
                .iter()
                .filter(|(s, e, _)| *s < to && *e > from)
                .map(|(s, e, style)| ((*s).max(from) - from, (*e).min(to) - from, *style))
                .collect()
        };
        let mut first = target.clone();
        first.parts = vec![Part {
            text: left.to_owned(),
            spans: clip(0, left.len()),
            ..part.clone()
        }];
        let mut second = target.clone();
        new.clone_into(&mut second.id);
        second.parts = vec![Part {
            text: text[right_start..].to_owned(),
            spans: clip(right_start, text.len()),
            ..part.clone()
        }];
        self.blocks[place] = first;
        self.blocks.insert(place + 1, second);
        self.rehome();
        Ok(())
    }

    fn merge(&mut self, block: &str) -> Result<(), String> {
        let at = self.index(block)?;
        if at == 0 {
            return Err("the first block has nothing above it to merge into".into());
        }
        let (above, this) = (&self.blocks[at - 1], &self.blocks[at]);
        if above.kind != Kind::Para || this.kind != Kind::Para {
            return Err("only paragraphs merge".into());
        }
        let (a, b) = (&above.parts[0], &this.parts[0]);
        let left = a.text.trim_end();
        let lead = b.text.len() - b.text.trim_start().len();
        let right = &b.text[lead..];
        let offset = left.len() + 1;
        let mut spans: Vec<_> = a
            .spans
            .iter()
            .filter(|(s, _, _)| *s < left.len())
            .map(|(s, e, style)| (*s, (*e).min(left.len()), *style))
            .collect();
        for (s, e, style) in &b.spans {
            if *e > lead {
                spans.push(((*s).max(lead) - lead + offset, e - lead + offset, *style));
            }
        }
        let merged = Part {
            text: format!("{left} {right}"),
            spans,
            ..a.clone()
        };
        self.blocks[at - 1].parts = vec![merged];
        self.blocks.remove(at);
        for mark in &mut self.marks {
            if mark.block == block {
                mark.block.clone_from(&self.blocks[at - 1].id);
            }
        }
        self.rehome();
        Ok(())
    }
}

// ---------------------------------------------------------------- selection

/// The sentence around byte `position`, as a byte range of `text`.
pub fn sentence_around(text: &str, position: usize) -> (usize, usize) {
    let position = position.min(text.len());
    let mut start = 0;
    let mut end = text.len();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (index, &(at, ch)) in chars.iter().enumerate() {
        if !matches!(ch, '.' | '!' | '?') {
            continue;
        }
        let mut next = index + 1;
        while next < chars.len() && matches!(chars[next].1, '"' | '\'' | ')' | '*' | '_') {
            next += 1;
        }
        let boundary = chars.get(next).is_none_or(|(_, c)| c.is_whitespace());
        if !boundary {
            continue;
        }
        let after = chars.get(next).map_or(text.len(), |(i, _)| *i);
        if after <= position {
            start = chars[next..]
                .iter()
                .find(|(_, c)| !c.is_whitespace())
                .map_or(text.len(), |(i, _)| *i);
        } else if at >= position {
            end = chars
                .get(next - 1)
                .map_or(text.len(), |(i, c)| i + c.len_utf8());
            break;
        }
    }
    (start.min(end), end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(id: &str, text: &str) -> Block {
        Block::text(id, Kind::Para, text)
    }

    fn base() -> Vec<Block> {
        let mut first = para("a", "First paragraph here, with bold words in it.");
        let at = first.parts[0].text.find("bold words").unwrap();
        first.parts[0].spans = vec![(at, at + "bold words".len(), Style::Bold)];
        vec![
            first,
            para("b", "Second paragraph is short."),
            para("c", "Third closes the section."),
        ]
    }

    fn texts(work: &Working) -> Vec<String> {
        work.blocks
            .iter()
            .map(|b| b.parts[0].text.clone())
            .collect()
    }

    #[test]
    fn move_and_delete_reorder_the_working_copy() {
        let ops = vec![
            Op::Move {
                block: "c".into(),
                after: None,
            },
            Op::Delete { block: "b".into() },
        ];
        let work = Working::replay(&base(), &ops).unwrap();
        let ids: Vec<_> = work.blocks.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["c", "a"]);
    }

    #[test]
    fn split_trims_the_cut_and_carries_spans_to_both_halves() {
        let at = base()[0].parts[0].text.find("words").unwrap();
        let ops = vec![Op::Split {
            block: "a".into(),
            at,
            new: "d1".into(),
        }];
        let work = Working::replay(&base(), &ops).unwrap();
        assert_eq!(texts(&work)[0], "First paragraph here, with bold");
        assert_eq!(texts(&work)[1], "words in it.");
        let bold = base()[0].parts[0].text.find("bold").unwrap();
        assert_eq!(
            work.blocks[0].parts[0].spans,
            vec![(bold, bold + 4, Style::Bold)]
        );
        assert_eq!(work.blocks[1].parts[0].spans, vec![(0, 5, Style::Bold)]);
        assert_eq!(work.blocks[1].id, "d1");
    }

    #[test]
    fn split_refuses_edges_other_kinds_and_reused_ids() {
        for (at, new) in [(0, "x"), (9999, "x"), (5, "b")] {
            let op = Op::Split {
                block: "a".into(),
                at,
                new: new.into(),
            };
            assert!(Working::replay(&base(), &[op]).is_err());
        }
        let mut blocks = base();
        blocks[0].kind = Kind::List;
        let op = Op::Split {
            block: "a".into(),
            at: 5,
            new: "x".into(),
        };
        assert!(Working::replay(&blocks, &[op]).is_err());
    }

    #[test]
    fn merge_joins_with_one_space_and_shifts_the_second_blocks_spans() {
        let mut blocks = base();
        blocks[1].parts[0].spans = vec![(0, 6, Style::Italic)];
        let work = Working::replay(&blocks, &[Op::Merge { block: "b".into() }]).unwrap();
        assert_eq!(
            work.blocks[0].parts[0].text,
            "First paragraph here, with bold words in it. Second paragraph is short."
        );
        let start = "First paragraph here, with bold words in it. ".len();
        assert!(work.blocks[0].parts[0]
            .spans
            .contains(&(start, start + 6, Style::Italic)));
        assert!(Working::replay(&blocks, &[Op::Merge { block: "a".into() }]).is_err());
    }

    #[test]
    fn edit_and_insert_follow_the_gateways_rules() {
        let ops = vec![
            Op::Edit {
                block: "b".into(),
                text: "  Retyped.  ".into(),
            },
            Op::Insert {
                after: Some("a".into()),
                text: "New.".into(),
                new: "d1".into(),
            },
        ];
        let work = Working::replay(&base(), &ops).unwrap();
        assert_eq!(
            texts(&work)[..3],
            [
                "First paragraph here, with bold words in it.",
                "New.",
                "Retyped."
            ]
        );
        for text in ["one\n\ntwo", "  "] {
            let op = Op::Edit {
                block: "a".into(),
                text: text.into(),
            };
            assert!(Working::replay(&base(), &[op]).is_err());
        }
    }

    fn mark(block: &str, text: &str) -> Op {
        Op::Mark {
            block: block.into(),
            part: 0,
            start: 0,
            end: 1,
            text: text.into(),
            ask: Ask::Tighten,
            note: String::new(),
        }
    }

    #[test]
    fn marks_follow_their_words_through_split_merge_and_delete() {
        let at = base()[0].parts[0].text.find("with").unwrap();
        let ops = vec![
            mark("a", "in it"),
            Op::Split {
                block: "a".into(),
                at,
                new: "d1".into(),
            },
        ];
        let work = Working::replay(&base(), &ops).unwrap();
        assert_eq!(
            work.marks,
            vec![Mark {
                block: "d1".into(),
                excerpt: "in it".into(),
                ask: Ask::Tighten
            }]
        );

        let ops = vec![mark("c", "closes"), Op::Merge { block: "c".into() }];
        let work = Working::replay(&base(), &ops).unwrap();
        assert_eq!(work.marks[0].block, "b");

        let ops = vec![mark("b", "short"), Op::Delete { block: "b".into() }];
        assert!(Working::replay(&base(), &ops).unwrap().marks.is_empty());
        assert!(Working::replay(&base(), &[mark("a", "absent words")]).is_err());
    }

    #[test]
    fn ops_survive_a_trip_through_json() {
        let ops = vec![
            mark("a", "bold words"),
            Op::Note {
                block: "b".into(),
                note: "drags".into(),
            },
            Op::Want {
                after: None,
                note: "an example".into(),
            },
            Op::Move {
                block: "c".into(),
                after: Some("a".into()),
            },
            Op::Split {
                block: "a".into(),
                at: 5,
                new: "d1".into(),
            },
            Op::Merge { block: "b".into() },
            Op::Delete { block: "c".into() },
            Op::Edit {
                block: "a".into(),
                text: "Quote \" and \\ and \n newline".into(),
            },
            Op::Insert {
                after: None,
                text: "Hi".into(),
                new: "d2".into(),
            },
        ];
        assert_eq!(ops_from_json(&ops_to_json(&ops)), Some(ops));
        assert_eq!(ops_from_json("{}"), None);
        assert_eq!(ops_from_json(r#"[{"op":"explode"}]"#), None);
    }

    #[test]
    fn blocks_decode_every_kind_from_the_gateways_json() {
        let json = r#"[
          {"id":"p1","kind":"para","text":"Pass. Your total","spans":[[0,4,"b"]]},
          {"id":"h1","kind":"heading","level":3,"text":"Sub","spans":[]},
          {"id":"c1","kind":"callout","label":"note","title":"Designer's Note","parts":[{"text":"One","spans":[]},{"text":"Two","spans":[[0,3,"i"]]}]},
          {"id":"l1","kind":"list","parts":[{"text":"A","spans":[],"depth":0,"ordered":false},{"text":"B","spans":[],"depth":1,"ordered":true}]},
          {"id":"t1","kind":"table","rows":[{"header":true,"cells":["A","B"]},{"header":false,"cells":["1","2"]}]},
          {"id":"k1","kind":"code","text":"x = 1"},
          {"id":"r1","kind":"hr"},
          {"id":"m1","kind":"marker","text":"DECISION NEEDED"}
        ]"#;
        let blocks = blocks_from(&kobo_json::parse(json).unwrap()).unwrap();
        assert_eq!(blocks.len(), 8);
        assert_eq!(blocks[0].parts[0].spans, vec![(0, 4, Style::Bold)]);
        assert_eq!(blocks[1].level, 3);
        assert_eq!(blocks[2].label, "note");
        assert_eq!(blocks[2].parts.len(), 2);
        assert_eq!(blocks[3].parts[1].depth, 1);
        assert!(blocks[3].parts[1].ordered);
        assert_eq!(blocks[4].rows[0].cells, ["A", "B"]);
        assert!(blocks[6].parts.is_empty());
        assert_eq!(blocks[5].snippet(40), "x = 1");
    }

    #[test]
    fn bad_spans_and_unknown_kinds_are_rejected_not_trusted() {
        let bad_span = r#"{"id":"p","kind":"para","text":"café","spans":[[0,4,"b"]]}"#;
        let block = block_from(&kobo_json::parse(bad_span).unwrap()).unwrap();
        assert!(
            block.parts[0].spans.is_empty(),
            "span ends inside a character"
        );
        assert!(block_from(&kobo_json::parse(r#"{"id":"p","kind":"mystery"}"#).unwrap()).is_none());
    }

    #[test]
    fn golden_vectors_match_the_gateway_byte_for_byte() {
        let golden = kobo_json::parse(include_str!("../tests/golden.json")).unwrap();
        let base = blocks_from(golden.get("base").unwrap()).unwrap();
        let vectors = golden.get("vectors").unwrap().as_array().unwrap();
        assert!(vectors.len() >= 10);
        for vector in vectors {
            let name = vector.get("name").unwrap().as_str().unwrap();
            let ops: Vec<Op> = vector
                .get("ops")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|op| {
                    Op::from_value(op).unwrap_or_else(|| panic!("{name}: op does not decode"))
                })
                .collect();
            let expect = vector.get("expect").unwrap();
            let want = blocks_from(expect.get("blocks").unwrap()).unwrap();
            let got = Working::replay(&base, &ops).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(got.blocks, want, "{name}: blocks differ");
            let marks: Vec<(String, String, String)> = got
                .marks
                .iter()
                .map(|m| (m.block.clone(), m.excerpt.clone(), m.ask.name().to_owned()))
                .collect();
            let wanted: Vec<(String, String, String)> = expect
                .get("marks")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|m| {
                    let get = |k: &str| m.get(k).unwrap().as_str().unwrap().to_owned();
                    (get("block"), get("excerpt"), get("ask"))
                })
                .collect();
            assert_eq!(marks, wanted, "{name}: marks differ");
        }
    }

    #[test]
    fn sentences_are_found_around_a_held_word() {
        let text =
            "You roll two dice. The host sets 6, weighing the risk! Then the roll lands. Done";
        let at = |word: &str| text.find(word).unwrap();
        assert_eq!(
            &text[sentence_around(text, at("host")).0..sentence_around(text, at("host")).1],
            "The host sets 6, weighing the risk!"
        );
        let (s, e) = sentence_around(text, at("roll two"));
        assert_eq!(&text[s..e], "You roll two dice.");
        let (s, e) = sentence_around(text, at("Done"));
        assert_eq!(&text[s..e], "Done");
        let (s, e) = sentence_around(text, at("lands"));
        assert_eq!(&text[s..e], "Then the roll lands.");
    }
}
