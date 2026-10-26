//! Blocks on a panel that cannot scroll.
//!
//! A page is a list of `Item`s: a whole block, a run of one paragraph, or a run
//! of a table's rows. Pages are filled by measurement, adding items until the
//! renderer reports the page no longer fits, and a block too tall for a page
//! on its own is split at a sentence (or a row), never clipped.

use crate::model::{Block, Kind, Style, Working};
use kobo_sdk::{BannerLevel, ScreenBuilder};
use kobo_ui::{ParagraphPresentation, RichTextSpan, TableRow, TextPresentation};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Item {
    /// A block drawn as one unit: heading, rule, marker, code, or a short table.
    Whole(usize),
    /// Bytes `from..to` of one part of a prose block.
    Text {
        block: usize,
        part: usize,
        from: usize,
        to: usize,
    },
    /// Rows `from..to` of a table (its header row repeats on every page).
    Rows {
        block: usize,
        from: usize,
        to: usize,
    },
}

impl Item {
    pub const fn block(self) -> usize {
        match self {
            Self::Whole(block) | Self::Text { block, .. } | Self::Rows { block, .. } => block,
        }
    }
}

/// The atoms of one block.
pub fn atoms_of(work: &Working, block: usize) -> Vec<Item> {
    atoms(work)
        .into_iter()
        .filter(|item| item.block() == block)
        .collect()
}

pub fn atoms(work: &Working) -> Vec<Item> {
    let mut items = Vec::new();
    for (index, block) in work.blocks.iter().enumerate() {
        match block.kind {
            Kind::Table if !block.rows.is_empty() => {
                items.push(Item::Rows {
                    block: index,
                    from: 0,
                    to: block.rows.len(),
                });
            }
            Kind::Table => {}
            Kind::Para | Kind::Callout | Kind::Quote | Kind::List => {
                for (part, p) in block.parts.iter().enumerate() {
                    if !p.text.trim().is_empty() {
                        items.push(Item::Text {
                            block: index,
                            part,
                            from: 0,
                            to: p.text.len(),
                        });
                    }
                }
            }
            Kind::Heading | Kind::Code | Kind::Hr | Kind::Marker => items.push(Item::Whole(index)),
        }
    }
    items
}

// ----------------------------------------------------------------- drawing

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // one flag per renderer style, as TextPresentation has
struct Flags {
    strong: bool,
    emphasis: bool,
    underline: bool,
    highlighted: bool,
}

impl Flags {
    const fn any(self) -> bool {
        self.strong || self.emphasis || self.underline || self.highlighted
    }
}

/// Overlapping styled ranges flattened into the non-overlapping runs the
/// renderer takes.
fn runs(len: usize, ranges: &[(usize, usize, Flags)]) -> Vec<RichTextSpan> {
    let mut cuts: Vec<usize> = vec![0, len];
    for (start, end, _) in ranges {
        cuts.push((*start).min(len));
        cuts.push((*end).min(len));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out: Vec<RichTextSpan> = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let mut flags = Flags::default();
        for (s, e, f) in ranges {
            if *s <= start && *e >= end {
                flags.strong |= f.strong;
                flags.emphasis |= f.emphasis;
                flags.underline |= f.underline;
                flags.highlighted |= f.highlighted;
            }
        }
        if !flags.any() {
            continue;
        }
        let presentation = TextPresentation {
            strong: flags.strong,
            emphasis: flags.emphasis,
            underline: flags.underline,
            highlighted: flags.highlighted,
            ..TextPresentation::default()
        };
        match out.last_mut() {
            Some(last) if last.end == start && last.presentation == presentation => last.end = end,
            _ => out.push(RichTextSpan {
                start,
                end,
                presentation,
            }),
        }
    }
    out
}

fn style_flags(style: Style) -> Flags {
    match style {
        Style::Bold => Flags {
            strong: true,
            ..Flags::default()
        },
        Style::Italic => Flags {
            emphasis: true,
            ..Flags::default()
        },
        Style::BoldItalic => Flags {
            strong: true,
            emphasis: true,
            ..Flags::default()
        },
        Style::Code => Flags {
            underline: true,
            ..Flags::default()
        },
    }
}

/// How a held word's coordinates are packed: block, part and the width of any
/// list marker drawn before the words.
pub const fn pack(block: usize, part: usize, prefix: usize) -> u64 {
    ((block as u64) << 32) | ((part as u64) << 16) | prefix as u64
}

pub const fn unpack(context: u64) -> (usize, usize, usize) {
    (
        (context >> 32) as usize,
        ((context >> 16) & 0xFFFF) as usize,
        (context & 0xFFFF) as usize,
    )
}

fn list_prefix(block: &Block, part: usize) -> String {
    let p = &block.parts[part];
    let indent = "  ".repeat(usize::from(p.depth));
    if p.ordered {
        let number = block.parts[..=part]
            .iter()
            .filter(|q| q.ordered && q.depth == p.depth)
            .count();
        format!("{indent}{number}. ")
    } else {
        format!("{indent}- ")
    }
}

fn label_of(block: &Block) -> String {
    if !block.title.is_empty() {
        return block.title.clone();
    }
    let mut chars = block.label.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

fn table_rows(block: &Block, from: usize, to: usize) -> Vec<TableRow> {
    let mut rows = Vec::new();
    if from > 0 && block.rows.first().is_some_and(|r| r.header) {
        rows.push(TableRow {
            header: true,
            cells: block.rows[0].cells.clone(),
        });
    }
    let end = to.min(block.rows.len());
    for row in block.rows.get(from.min(end)..end).unwrap_or_default() {
        rows.push(TableRow {
            header: row.header,
            cells: row.cells.clone(),
        });
    }
    rows
}

pub fn render(mut screen: ScreenBuilder, work: &Working, item: Item, hold: bool) -> ScreenBuilder {
    match item {
        Item::Whole(index) => {
            let block = &work.blocks[index];
            let text = block
                .parts
                .first()
                .map(|p| p.text.clone())
                .unwrap_or_default();
            match block.kind {
                Kind::Heading => screen.section(text),
                Kind::Code => screen.quote(1, text),
                Kind::Hr => screen.divider(),
                Kind::Marker => screen.banner(BannerLevel::Info, text),
                _ => screen,
            }
        }
        Item::Rows {
            block: index,
            from,
            to,
        } => {
            let block = &work.blocks[index];
            screen.table(table_rows(block, from, to), Vec::new())
        }
        Item::Text {
            block: index,
            part,
            from,
            to,
        } => {
            let block = &work.blocks[index];
            let source = &block.parts[part];
            let first = from == 0;
            if matches!(block.kind, Kind::Callout) && part == 0 && first {
                screen = screen.secondary(label_of(block));
            }
            let prefix = if block.kind == Kind::List && first {
                list_prefix(block, part)
            } else {
                String::new()
            };
            let body = source.text[from..to].trim_end();
            let text = format!("{prefix}{body}");
            let shift = |at: usize| at.saturating_sub(from) + prefix.len();
            let mut ranges: Vec<(usize, usize, Flags)> = Vec::new();
            if matches!(block.kind, Kind::Callout | Kind::Quote) {
                ranges.push((
                    0,
                    text.len(),
                    Flags {
                        emphasis: true,
                        ..Flags::default()
                    },
                ));
            }
            for (s, e, style) in &source.spans {
                if *s < to && *e > from {
                    ranges.push((shift(*s), shift(*e).min(text.len()), style_flags(*style)));
                }
            }
            for mark in work.marks_in(&block.id) {
                if let Some(at) = source.text.find(&mark.excerpt) {
                    let end = at + mark.excerpt.len();
                    if at < to && end > from {
                        ranges.push((
                            shift(at.max(from)),
                            shift(end.min(to)).min(text.len()),
                            Flags {
                                highlighted: true,
                                ..Flags::default()
                            },
                        ));
                    }
                }
            }
            let presentation = ParagraphPresentation {
                margin_after_em: 1,
                ..ParagraphPresentation::default()
            };
            let spans = runs(text.len(), &ranges);
            let offset = u32::try_from(from + prefix.len()).unwrap_or(u32::MAX);
            if hold {
                screen.selectable_rich_text_linking(
                    text,
                    spans,
                    presentation,
                    pack(index, part, prefix.len()),
                    offset,
                    std::iter::empty::<(&str, usize, usize)>(),
                )
            } else {
                screen.rich_text(text, spans, presentation)
            }
        }
    }
}

// -------------------------------------------------------------- pagination

/// Byte offsets inside `from..to` where a new sentence starts.
pub fn sentence_starts(text: &str, from: usize, to: usize) -> Vec<usize> {
    let mut starts = Vec::new();
    let slice = &text[from..to];
    let chars: Vec<(usize, char)> = slice.char_indices().collect();
    for (index, &(_, ch)) in chars.iter().enumerate() {
        if !matches!(ch, '.' | '!' | '?') {
            continue;
        }
        let mut next = index + 1;
        while next < chars.len() && matches!(chars[next].1, '"' | '\'' | ')' | '*' | '_') {
            next += 1;
        }
        if chars.get(next).is_some_and(|(_, c)| c.is_whitespace()) {
            if let Some((at, _)) = chars[next..].iter().find(|(_, c)| !c.is_whitespace()) {
                starts.push(from + at);
            }
        }
    }
    starts
}

fn word_starts(text: &str, from: usize, to: usize) -> Vec<usize> {
    let slice = &text[from..to];
    let mut starts = Vec::new();
    let mut previous_space = false;
    for (at, ch) in slice.char_indices() {
        if !ch.is_whitespace() && previous_space {
            starts.push(from + at);
        }
        previous_space = ch.is_whitespace();
    }
    starts
}

/// The largest prefix of `candidates` that satisfies `fits`, by bisection.
fn largest(candidates: &[usize], mut fits: impl FnMut(usize) -> bool) -> Option<usize> {
    let (mut low, mut high) = (0, candidates.len());
    let mut best = None;
    while low < high {
        let mid = usize::midpoint(low, high);
        if fits(candidates[mid]) {
            best = Some(candidates[mid]);
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    best
}

/// Splits an item that does not fit a page alone into a head that does and the
/// remainder. A whole block cannot be split and is returned as it is.
pub fn split(
    work: &Working,
    item: Item,
    fits: &mut impl FnMut(Item) -> bool,
) -> (Item, Option<Item>) {
    match item {
        Item::Whole(_) => (item, None),
        Item::Rows { block, from, to } => {
            let candidates: Vec<usize> = (from + 1..to).collect();
            let cut = largest(&candidates, |end| {
                fits(Item::Rows {
                    block,
                    from,
                    to: end,
                })
            })
            .unwrap_or(from + 1)
            .min(to);
            let tail = (cut < to).then_some(Item::Rows {
                block,
                from: cut,
                to,
            });
            (
                Item::Rows {
                    block,
                    from,
                    to: cut,
                },
                tail,
            )
        }
        Item::Text {
            block,
            part,
            from,
            to,
        } => {
            let text = &work.blocks[block].parts[part].text;
            let mut candidates = sentence_starts(text, from, to);
            let head = |end: usize| Item::Text {
                block,
                part,
                from,
                to: end,
            };
            let first_fits = candidates.first().is_some_and(|&end| fits(head(end)));
            if !first_fits {
                candidates = word_starts(text, from, to);
            }
            let cut =
                largest(&candidates, |end| fits(head(end))).or_else(|| candidates.first().copied());
            match cut {
                Some(end) if end < to => (
                    head(end),
                    Some(Item::Text {
                        block,
                        part,
                        from: end,
                        to,
                    }),
                ),
                _ => (item, None),
            }
        }
    }
}

/// Splits an item that does not fit beside what is already on the page, if a
/// worthwhile head would: two or more table rows, or whole sentences. Returns
/// the head that fits and the rest, or nothing when it is better to start a
/// new page than to leave a stub behind.
fn fill(work: &Working, item: Item, fits: &mut impl FnMut(Item) -> bool) -> Option<(Item, Item)> {
    match item {
        Item::Whole(_) => None,
        Item::Rows { block, from, to } => {
            let candidates: Vec<usize> = (from + 2..to).collect();
            let cut = largest(&candidates, |end| {
                fits(Item::Rows {
                    block,
                    from,
                    to: end,
                })
            })?;
            Some((
                Item::Rows {
                    block,
                    from,
                    to: cut,
                },
                Item::Rows {
                    block,
                    from: cut,
                    to,
                },
            ))
        }
        Item::Text {
            block,
            part,
            from,
            to,
        } => {
            let text = &work.blocks[block].parts[part].text;
            let candidates = sentence_starts(text, from, to);
            let cut = largest(&candidates, |end| {
                fits(Item::Text {
                    block,
                    part,
                    from,
                    to: end,
                })
            })?;
            Some((
                Item::Text {
                    block,
                    part,
                    from,
                    to: cut,
                },
                Item::Text {
                    block,
                    part,
                    from: cut,
                    to,
                },
            ))
        }
    }
}

/// Fills pages greedily. `fits` says whether a candidate page is drawn whole.
pub fn paginate(work: &Working, mut fits: impl FnMut(&[Item]) -> bool) -> Vec<Vec<Item>> {
    let mut queue: VecDeque<Item> = atoms(work).into();
    let mut pages: Vec<Vec<Item>> = Vec::new();
    let mut page: Vec<Item> = Vec::new();
    while let Some(item) = queue.pop_front() {
        page.push(item);
        if fits(&page) {
            continue;
        }
        page.pop();
        if page.is_empty() {
            let (head, tail) = split(work, item, &mut |alone| fits(&[alone]));
            if let Some(tail) = tail {
                queue.push_front(tail);
            }
            pages.push(vec![head]);
        } else {
            let filled = fill(work, item, &mut |part: Item| {
                let mut candidate = page.clone();
                candidate.push(part);
                fits(&candidate)
            });
            if let Some((head, tail)) = filled {
                page.push(head);
                queue.push_front(tail);
            } else {
                queue.push_front(item);
            }
            pages.push(std::mem::take(&mut page));
        }
    }
    if !page.is_empty() {
        pages.push(page);
    }
    pages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Block, Part, Row};

    fn work(blocks: Vec<Block>) -> Working {
        Working {
            blocks,
            ..Working::default()
        }
    }

    #[test]
    fn overlapping_styles_flatten_into_runs() {
        let bold = Flags {
            strong: true,
            ..Flags::default()
        };
        let wash = Flags {
            highlighted: true,
            ..Flags::default()
        };
        let out = runs(20, &[(0, 10, bold), (5, 15, wash)]);
        let shape: Vec<_> = out
            .iter()
            .map(|r| {
                (
                    r.start,
                    r.end,
                    r.presentation.strong,
                    r.presentation.highlighted,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                (0, 5, true, false),
                (5, 10, true, true),
                (10, 15, false, true)
            ]
        );
        assert!(runs(5, &[]).is_empty());
    }

    #[test]
    fn adjacent_runs_with_the_same_look_merge() {
        let bold = Flags {
            strong: true,
            ..Flags::default()
        };
        let out = runs(10, &[(0, 4, bold), (4, 8, bold)]);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].start, out[0].end), (0, 8));
    }

    #[test]
    fn context_packing_round_trips() {
        for (b, p, x) in [(0, 0, 0), (7, 3, 4), (65_000, 40, 12)] {
            assert_eq!(unpack(pack(b, p, x)), (b, p, x));
        }
    }

    #[test]
    fn sentences_and_words_are_found_for_splitting() {
        let text = "One two. Three four! Five? Six.";
        assert_eq!(sentence_starts(text, 0, text.len()), vec![9, 21, 27]);
        assert_eq!(word_starts("a bc  d", 0, 7), vec![2, 6]);
        assert_eq!(largest(&[1, 2, 3, 4], |n| n <= 3), Some(3));
        assert_eq!(largest(&[5, 6], |n| n <= 3), None);
    }

    #[test]
    fn a_page_takes_as_much_as_fits_and_the_rest_flows_on() {
        let blocks = (0..7)
            .map(|n| Block::text(&format!("b{n}"), Kind::Para, &format!("Paragraph {n}.")))
            .collect();
        let pages = paginate(&work(blocks), |page| page.len() <= 3);
        let sizes: Vec<_> = pages.iter().map(Vec::len).collect();
        assert_eq!(sizes, [3, 3, 1]);
        let order: Vec<_> = pages.iter().flatten().map(|i| i.block()).collect();
        assert_eq!(order, (0..7).collect::<Vec<_>>());
    }

    #[test]
    fn an_oversize_paragraph_splits_on_sentences_without_losing_a_byte() {
        let text = "Alpha one. Beta two. Gamma three. Delta four. Epsilon five.";
        let w = work(vec![Block::text("p", Kind::Para, text)]);
        // A page holds at most 25 bytes of text.
        let mut fits = |page: &[Item]| {
            page.iter().all(|item| match item {
                Item::Text { from, to, .. } => to - from <= 25,
                _ => true,
            })
        };
        let pages = paginate(&w, &mut fits);
        assert!(pages.len() >= 3, "{pages:?}");
        let mut rebuilt = String::new();
        for item in pages.iter().flatten() {
            let Item::Text { from, to, .. } = *item else {
                panic!()
            };
            rebuilt.push_str(&text[from..to]);
        }
        assert_eq!(
            rebuilt.split_whitespace().collect::<Vec<_>>().join(" "),
            text
        );
        let heads: Vec<_> = pages
            .iter()
            .map(|p| match p[0] {
                Item::Text { from, .. } => &text[from..from + 5],
                _ => "",
            })
            .collect();
        assert_eq!(heads[0], "Alpha");
        assert!(heads.contains(&"Gamma"), "sentences stay whole: {heads:?}");
    }

    #[test]
    fn a_table_that_will_not_fit_beside_a_paragraph_fills_the_rest_of_the_page() {
        let mut table = Block::text("t", Kind::Table, "");
        table.parts.clear();
        table.rows = (0..9)
            .map(|n| Row {
                header: n == 0,
                cells: vec![format!("a{n}"), format!("b{n}")],
            })
            .collect();
        let w = work(vec![Block::text("p", Kind::Para, "Intro."), table]);
        // A page holds the paragraph plus at most four table rows, or six rows alone.
        let pages = paginate(&w, |page| {
            let rows: usize = page
                .iter()
                .map(|i| match i {
                    Item::Rows { from, to, .. } => to - from,
                    _ => 0,
                })
                .sum();
            let has_text = page.iter().any(|i| matches!(i, Item::Text { .. }));
            rows <= if has_text { 4 } else { 6 }
        });
        assert_eq!(
            pages[0].len(),
            2,
            "the first page carries the paragraph and table rows"
        );
        assert!(
            matches!(pages[0][1], Item::Rows { from: 0, to: 4, .. }),
            "{:?}",
            pages[0]
        );
        let covered: usize = pages
            .iter()
            .flatten()
            .map(|i| match i {
                Item::Rows { from, to, .. } => to - from,
                _ => 0,
            })
            .sum();
        assert_eq!(covered, 9, "every row is on some page");
    }

    #[test]
    fn a_stub_is_never_left_behind() {
        // Only one table row would fit beside the paragraph: start a new page instead.
        let mut table = Block::text("t", Kind::Table, "");
        table.parts.clear();
        table.rows = (0..6)
            .map(|n| Row {
                header: n == 0,
                cells: vec![format!("a{n}")],
            })
            .collect();
        let w = work(vec![Block::text("p", Kind::Para, "Intro."), table]);
        let pages = paginate(&w, |page| {
            let rows: usize = page
                .iter()
                .map(|i| match i {
                    Item::Rows { from, to, .. } => to - from,
                    _ => 0,
                })
                .sum();
            let has_text = page.iter().any(|i| matches!(i, Item::Text { .. }));
            rows <= if has_text { 1 } else { 6 }
        });
        assert_eq!(pages[0].len(), 1);
        assert!(matches!(pages[1][0], Item::Rows { from: 0, to: 6, .. }));
    }

    #[test]
    fn a_long_unpunctuated_run_falls_back_to_words() {
        let text = "one two three four five six seven eight nine ten";
        let w = work(vec![Block::text("p", Kind::Para, text)]);
        let pages = paginate(&w, |page| {
            page.iter()
                .all(|i| matches!(i, Item::Text { from, to, .. } if to - from <= 15))
        });
        assert!(pages.len() >= 4);
    }

    #[test]
    fn tables_split_by_rows_and_repeat_their_header() {
        let mut table = Block::text("t", Kind::Table, "");
        table.parts.clear();
        table.rows = (0..6)
            .map(|n| Row {
                header: n == 0,
                cells: vec![format!("a{n}"), format!("b{n}")],
            })
            .collect();
        let w = work(vec![table.clone()]);
        let pages = paginate(&w, |page| {
            page.iter()
                .all(|i| matches!(i, Item::Rows { from, to, .. } if to - from <= 2))
        });
        assert_eq!(pages.len(), 3);
        let second = table_rows(&table, 2, 4);
        assert_eq!(second.len(), 3);
        assert!(second[0].header);
        assert_eq!(second[1].cells[0], "a2");
    }

    #[test]
    fn every_kind_has_an_atom_and_empty_parts_do_not() {
        let mut empty = Block::text("e", Kind::Para, "   ");
        empty.parts[0].text = "   ".into();
        let mut list = Block::text("l", Kind::List, "x");
        list.parts = vec![
            Part {
                text: "A".into(),
                ..Part::default()
            },
            Part {
                text: "B".into(),
                ..Part::default()
            },
        ];
        let w = work(vec![
            Block::text("h", Kind::Heading, "H"),
            empty,
            list,
            Block::text("r", Kind::Hr, ""),
        ]);
        let kinds: Vec<_> = atoms(&w).iter().map(|i| i.block()).collect();
        assert_eq!(kinds, [0, 2, 2, 3]);
    }
}
