//! Paged history for #15, behind `--paged`: an offset table over the session log, the
//! panel's folds from one streaming pass, and a window of rendered pages around the
//! viewport. See PAGING.md.
//!
//! A page is a run of lines inside one turn, cut only where no tool group, call,
//! reasoning or message is open, so a page folds on its own and its rows join its
//! neighbours' into exactly the rows the whole-file fold renders.
use crate::{Block, Fold, Item, Row, Turn, View, find_all, turn_rows};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

pub struct Page {
    /// first line, and one past the last
    pub first: usize,
    pub end: usize,
    /// the page starts its turn (bubble, the card's top edge), or ends it (bottom edge)
    head: bool,
    tail: bool,
    turn_ts: i64,
    calls_before: usize,
    /// the page starts right after a handoff band
    after_band: bool,
    /// the turn's earlier pages drew no blocks
    fresh: bool,
}

pub struct Pager {
    file: File,
    /// byte offset of every line, then the file's length
    pub offsets: Vec<u64>,
    /// first line of every turn
    pub turns: Vec<usize>,
    pub pages: Vec<Page>,
    /// rows per page at `key`: measured by rendering every page, keeping nothing
    pub counts: Vec<usize>,
    /// first row of each page, then the total
    starts: Vec<usize>,
    /// (width, every ledger open, query that opens groups) the counts are for
    key: Option<(usize, bool, String)>,
    find: String,
    /// search matches as (page, row in page, first cell, width)
    pub hits: Vec<(usize, usize, usize, usize)>,
    resident: HashMap<usize, (Fold, Vec<Row>)>,
    /// each completed handoff's line, and the context size the first usage after it reports,
    /// which may be pages later
    afters: HashMap<usize, u64>,
    /// groups the person opened, by (page, block), so a page loads as it was left
    opened: HashSet<(usize, usize)>,
    sid: String,
    pub resident_max: usize,
}

fn kind_sid(e: &Value) -> (&str, &str) {
    (
        e["kind"].as_str().unwrap_or(""),
        e["session_id"].as_str().unwrap_or(""),
    )
}

impl Pager {
    /// One streaming pass: the offset table, the pages, and the panel's folds. Returns
    /// the pager and the summary fold, which keeps the panel's totals and drops each
    /// event's text as soon as it is applied.
    pub fn open(path: &str, page_lines: usize) -> io::Result<(Pager, Fold)> {
        let file = File::open(path)?;
        let mut rd = BufReader::with_capacity(1 << 16, file.try_clone()?);
        let mut sum = Fold::default();
        let (mut offsets, mut turns, mut pages) = (vec![], vec![], vec![]);
        let mut cur: Option<Page> = None;
        let (mut open, mut in_group, mut calls) = (0i64, false, 0usize);
        // a handoff writing its note, and the completed handoff still waiting on its size after
        let (mut handing, mut waiting): (bool, Option<usize>) = (false, None);
        let mut afters = HashMap::new();
        let mut buf = String::new();
        let mut off = 0u64;
        let mut i = 0usize;
        loop {
            buf.clear();
            let n = rd.read_line(&mut buf)?;
            if n == 0 {
                break;
            }
            offsets.push(off);
            off += n as u64;
            let Ok(e) = serde_json::from_str::<Value>(&buf) else {
                i += 1;
                continue;
            };
            if sum.session_id.is_empty() {
                sum.session_id = e["session_id"].as_str().unwrap_or("").into();
            }
            let (kind, sid) = kind_sid(&e);
            if sid == sum.session_id {
                if kind == "turn_started" {
                    if let Some(mut p) = cur.take() {
                        p.end = i;
                        p.tail = true;
                        pages.push(p);
                    }
                    turns.push(i);
                    calls = 0;
                    cur = Some(Page {
                        first: i,
                        end: i,
                        head: true,
                        tail: false,
                        turn_ts: e["ts"].as_i64().unwrap_or(0),
                        calls_before: 0,
                        after_band: false,
                        fresh: false,
                    });
                } else if kind == "assistant_message_started"
                    && open == 0
                    && !in_group
                    && !handing
                    && cur.as_ref().is_some_and(|p| i - p.first >= page_lines)
                {
                    let mut p = cur.take().unwrap();
                    p.end = i;
                    let ts = p.turn_ts;
                    pages.push(p);
                    // the turn's blocks so far are still in the summary fold, which trims only their text
                    let blocks = sum.turns.last().map_or(&[][..], |t| &t.blocks[..]);
                    let (after_band, fresh) = (
                        matches!(blocks.last(), Some(Block::Handoff(_))),
                        blocks.is_empty(),
                    );
                    cur = Some(Page {
                        first: i,
                        end: i,
                        head: false,
                        tail: false,
                        turn_ts: ts,
                        calls_before: calls,
                        after_band,
                        fresh,
                    });
                }
                match kind {
                    "tool_call_requested" => {
                        open += 1;
                        calls += 1;
                        in_group = true;
                    }
                    "reasoning_started" => {
                        open += 1;
                        in_group = true;
                    }
                    "assistant_message_started" => open += 1,
                    "tool_call_completed" | "reasoning_completed" => open -= 1,
                    "assistant_message_completed" => {
                        open -= 1;
                        if !e["payload"]["text"].as_str().unwrap_or("").is_empty() {
                            in_group = false;
                        }
                    }
                    "steering_applied" | "interaction_resolved" | "turn_completed" => {
                        in_group = false
                    }
                    "handoff_started" => handing = true,
                    "handoff_completed" => {
                        handing = false;
                        waiting = (e["payload"]["outcome"].as_str().unwrap_or("completed")
                            == "completed")
                            .then_some(i);
                    }
                    _ => {}
                }
            }
            sum.apply(&e);
            if kind == "usage_recorded"
                && sid == sum.session_id
                && let Some(l) = waiting.take()
            {
                afters.insert(l, sum.ctx);
            }
            trim(&mut sum, kind, sid, e["action_id"].as_str().unwrap_or(""));
            i += 1;
        }
        offsets.push(off);
        if let Some(mut p) = cur.take() {
            p.end = i;
            p.tail = true;
            pages.push(p);
        }
        let sid = sum.session_id.clone();
        let pager = Pager {
            file,
            offsets,
            turns,
            pages,
            counts: vec![],
            starts: vec![0],
            key: None,
            find: String::new(),
            hits: vec![],
            resident: HashMap::new(),
            afters,
            opened: HashSet::new(),
            sid,
            resident_max: 0,
        };
        Ok((pager, sum))
    }

    /// Reads and folds one page: a range read by line.
    pub fn fold_page(&mut self, p: usize) -> Fold {
        let pg = &self.pages[p];
        let (a, b) = (self.offsets[pg.first], self.offsets[pg.end]);
        let mut f = Fold {
            session_id: self.sid.clone(),
            ..Default::default()
        };
        if !pg.head {
            f.turns.push(Turn {
                ts: pg.turn_ts,
                calls_before: pg.calls_before,
                after_band: pg.after_band,
                fresh: pg.fresh,
                ..Default::default()
            });
        }
        let mut bytes = vec![0u8; (b - a) as usize];
        self.file
            .seek(SeekFrom::Start(a))
            .and_then(|_| self.file.read_exact(&mut bytes))
            .expect("the log is readable");
        let first = pg.first;
        for (k, l) in bytes.split(|&c| c == b'\n').enumerate() {
            if let Ok(e) = serde_json::from_slice::<Value>(l) {
                f.apply(&e);
            }
            // the size after a handoff comes from a usage line that may be on a later page
            if let Some(&n) = self.afters.get(&(first + k)) {
                if let Some(h) = f.band() {
                    h.after = Some(n);
                }
                f.ho = None;
            }
        }
        if let Some(t) = f.turns.first_mut() {
            for &(_, bi) in self.opened.iter().filter(|x| x.0 == p) {
                if let Some(Block::Group(g)) = t.blocks.get_mut(bi) {
                    g.open = true;
                }
            }
        }
        f
    }
    fn render(&self, p: usize, f: &Fold, v: &View) -> Vec<Row> {
        let w = self.key.as_ref().map_or(80, |k| k.0);
        let pg = &self.pages[p];
        f.turns
            .first()
            .map_or_else(Vec::new, |t| turn_rows(p, t, w, v, pg.head, pg.tail))
    }
    fn restart(&mut self) {
        self.starts = std::iter::once(0)
            .chain(self.counts.iter().scan(0, |s, &c| {
                *s += c;
                Some(*s)
            }))
            .collect();
    }
    pub fn total(&self) -> usize {
        *self.starts.last().unwrap()
    }
    pub fn start_of(&self, p: usize) -> usize {
        self.starts[p]
    }
    /// The page holding row `r`.
    pub fn page_at(&self, r: usize) -> usize {
        self.starts[..self.pages.len()]
            .partition_point(|&s| s <= r)
            .saturating_sub(1)
    }

    /// Renders every page at the width, keeping only the row counts and, when `find` is
    /// set, the matches. Runs when the width, Ctrl+O or the query changes. Returns its time.
    pub fn sync(&mut self, w: usize, v: &View, find: &str) -> Option<Duration> {
        let key = (w, v.all_open, v.q.clone());
        let refind = find != self.find && !find.is_empty();
        self.find = find.to_string();
        if self.key.as_ref() == Some(&key) && !refind {
            return None;
        }
        let t = Instant::now();
        self.key = Some(key);
        self.resident.clear();
        self.counts.clear();
        self.hits.clear();
        for p in 0..self.pages.len() {
            let f = self.fold_page(p);
            let rows = self.render(p, &f, v);
            self.counts.push(rows.len());
            if !find.is_empty() {
                self.hits.extend(
                    find_all(&rows, find)
                        .into_iter()
                        .map(|(r, c, n)| (p, r, c, n)),
                );
            }
        }
        self.restart();
        Some(t.elapsed())
    }

    /// Keeps resident exactly the pages holding rows [lo, hi), loading what is missing.
    /// Returns how many pages it loaded.
    pub fn ensure(&mut self, lo: usize, hi: usize, v: &View) -> usize {
        if self.pages.is_empty() || hi <= lo {
            return 0;
        }
        let (p0, p1) = (self.page_at(lo), self.page_at(hi - 1));
        self.resident.retain(|p, _| (p0..=p1).contains(p));
        let mut n = 0;
        for p in p0..=p1 {
            if !self.resident.contains_key(&p) {
                let f = self.fold_page(p);
                let rows = self.render(p, &f, v);
                debug_assert_eq!(
                    rows.len(),
                    self.counts[p],
                    "a page renders to the rows it was counted at"
                );
                self.resident.insert(p, (f, rows));
                n += 1;
            }
        }
        self.resident_max = self.resident_max.max(self.resident.len());
        n
    }
    pub fn resident(&self, p: usize) -> bool {
        self.resident.contains_key(&p)
    }
    pub fn resident_count(&self) -> usize {
        self.resident.len()
    }
    /// A row of a resident page.
    pub fn row(&self, r: usize) -> &Row {
        let p = self.page_at(r);
        &self.resident[&p].1[r - self.starts[p]]
    }
    /// Rows [lo, hi], resident or not: a page that was dropped is read and rendered again.
    /// Returns the first row's index and the rows.
    pub fn rows(&mut self, lo: usize, hi: usize, v: &View) -> (usize, Vec<Row>) {
        let (p0, p1) = (self.page_at(lo), self.page_at(hi));
        let mut out = vec![];
        for p in p0..=p1 {
            match self.resident.get(&p) {
                Some((_, r)) => out.extend(r.iter().cloned()),
                None => {
                    let f = self.fold_page(p);
                    out.extend(self.render(p, &f, v));
                }
            }
        }
        (self.starts[p0], out)
    }
    /// Opens or closes a group; only its page's count changes.
    pub fn toggle(&mut self, p: usize, bi: usize, v: &View) {
        if !self.opened.remove(&(p, bi)) {
            self.opened.insert((p, bi));
        }
        if let Some((mut f, _)) = self.resident.remove(&p) {
            if let Some(Block::Group(g)) = f.turns.first_mut().and_then(|t| t.blocks.get_mut(bi)) {
                g.open = self.opened.contains(&(p, bi));
            }
            let rows = self.render(p, &f, v);
            self.counts[p] = rows.len();
            self.resident.insert(p, (f, rows));
            self.restart();
        }
    }
}

/// Drops what the panel never shows once an event has been applied: tool output, reply
/// and reasoning text, and a finished turn's blocks.
fn trim(f: &mut Fold, kind: &str, sid: &str, aid: &str) {
    if sid != f.session_id {
        return;
    }
    let Some(&(ti, bi, ii)) = f.at.get(aid) else {
        if kind == "turn_completed" {
            if let Some(t) = f.turns.last_mut() {
                *t = Turn {
                    ts: t.ts,
                    ..Default::default()
                };
            }
            f.at.clear();
            f.text_start.clear();
            f.text_dur.clear();
        }
        return;
    };
    match (kind, &mut f.turns[ti].blocks[bi]) {
        ("tool_call_completed", Block::Group(g)) => {
            if let Some(Item::C(c)) = g.items.get_mut(ii) {
                c.content = String::new();
                c.args = Value::Null;
            }
        }
        ("reasoning_completed", Block::Group(g)) => {
            if let Some(Item::R(r)) = g.items.get_mut(ii) {
                r.text = String::new();
            }
        }
        ("assistant_message_completed", Block::Text(t)) => *t = String::new(),
        _ => {}
    }
}

fn prefix(c: &[usize]) -> Vec<usize> {
    std::iter::once(0)
        .chain(c.iter().scan(0, |s, &x| {
            *s += x;
            Some(*s)
        }))
        .collect()
}
fn at(s: &[usize], r: usize) -> usize {
    s[..s.len() - 1]
        .partition_point(|&x| x <= r)
        .saturating_sub(1)
}
/// The scroll bar's thumb top, in cells, as the prototype draws it.
fn thumb(start: usize, total: usize, vh: usize) -> usize {
    if total <= vh {
        return 0;
    }
    let th = (vh * vh / total).max(1);
    (vh - th) * start / (total - vh)
}

/// Scroll bar option (b): each page's rows estimated from `weight` (its bytes or its
/// lines) at the ratio of rows to weight over the pages loaded so far, and corrected as
/// pages load. Opens at the bottom and scrolls to the top one row at a time, keeping the
/// top row still on screen. Returns the total's error at open (percent), the largest thumb
/// move in one step (cells), how many steps moved it more than one cell, and how many
/// moved it down while scrolling up, and the largest distance (cells) from where exact counts
/// would draw the thumb for the same row.
pub fn thumb_jumps(
    real: &[usize],
    weight: &[u64],
    vh: usize,
    m: usize,
) -> (f64, usize, usize, usize, usize) {
    let n = real.len();
    let real_total: usize = real.iter().sum();
    let mut known = vec![false; n];
    let (mut got, mut p) = (0, n);
    while p > 0 && got < vh * (1 + m) {
        p -= 1;
        known[p] = true;
        got += real[p];
    }
    let est = |known: &[bool]| -> Vec<usize> {
        let (r, w) = (0..n)
            .filter(|&i| known[i])
            .fold((0u64, 0u64), |(r, w), i| {
                (r + real[i] as u64, w + weight[i])
            });
        let ratio = r as f64 / w.max(1) as f64;
        (0..n)
            .map(|i| {
                if known[i] {
                    real[i]
                } else {
                    (ratio * weight[i] as f64).round() as usize
                }
            })
            .collect()
    };
    let s = prefix(&est(&known));
    let total = s[n];
    let open_err = (total as f64 - real_total as f64) * 100.0 / real_total.max(1) as f64;
    let top = total.saturating_sub(vh);
    let mut ap = at(&s, top);
    let mut off = top - s[ap];
    let mut prev = thumb(top, total, vh);
    let (mut maxj, mut big, mut back) = (0, 0, 0);
    // the thumb's largest distance from where exact counts would draw it for the same row
    let rs = prefix(real);
    let mut dev = prev.abs_diff(thumb(rs[ap] + off, real_total, vh));
    loop {
        if off > 0 {
            off -= 1;
        } else if ap > 0 {
            ap -= 1;
            known[ap] = true;
            off = real[ap].saturating_sub(1);
        } else {
            break;
        }
        let s = prefix(&est(&known));
        let start = s[ap] + off;
        let (lo, hi) = (
            start.saturating_sub(m * vh),
            (start + vh + m * vh).min(s[n]),
        );
        let end = (at(&s, hi.saturating_sub(1).max(lo)) + 1).min(known.len());
        known[at(&s, lo).min(end)..end].fill(true);
        let s = prefix(&est(&known));
        let t = thumb(s[ap] + off, s[n], vh);
        dev = dev.max(t.abs_diff(thumb(rs[ap] + off, real_total, vh)));
        let j = prev.abs_diff(t);
        maxj = maxj.max(j);
        big += (j > 1) as usize;
        back += (t > prev) as usize;
        prev = t;
    }
    (open_err, maxj, big, back, dev)
}

fn ms(d: Duration) -> String {
    format!("{:.3}", d.as_secs_f64() * 1000.0)
}
fn pct(v: &mut [Duration], q: f64) -> Duration {
    v.sort();
    v[((v.len() - 1) as f64 * q).round() as usize]
}

/// `--paging-bench`: the measurements that need no terminal, one run, as key<TAB>value.
/// `cw` is the conversation's text width and `vh` its height.
pub fn bench(
    path: &str,
    page_lines: usize,
    margins: &[usize],
    cw: usize,
    vh: usize,
    words: &[&str],
) -> io::Result<String> {
    use std::fmt::Write as _;
    let mut s = String::new();
    let mut v = View::default();
    // the first pass in a process pays for page faults and the allocator's growth, whichever
    // pass it is: one discarded pass, so every figure below is warm
    drop(Pager::open(path, page_lines)?);
    let t = Instant::now();
    let (mut pg, _sum) = Pager::open(path, page_lines)?;
    let _ = writeln!(s, "open_index_ms\t{}", ms(t.elapsed()));
    let _ = writeln!(s, "open_count_ms\t{}", ms(pg.sync(cw, &v, "").unwrap()));
    let _ = writeln!(
        s,
        "lines\t{}\nturns\t{}\npages\t{}\ntotal_rows\t{}",
        pg.offsets.len() - 1,
        pg.turns.len(),
        pg.pages.len(),
        pg.total()
    );
    let mut rows: Vec<usize> = pg.counts.clone();
    rows.sort();
    let _ = writeln!(
        s,
        "page_rows_median\t{}\npage_rows_max\t{}",
        rows[rows.len() / 2],
        rows[rows.len() - 1]
    );
    let _ = writeln!(
        s,
        "page_lines_max\t{}",
        pg.pages.iter().map(|p| p.end - p.first).max().unwrap_or(0)
    );
    let _ = writeln!(
        s,
        "page_bytes_max\t{}",
        pg.pages
            .iter()
            .map(|p| pg.offsets[p.end] - pg.offsets[p.first])
            .max()
            .unwrap_or(0)
    );
    // (a): exact counts; their cost again on a resize (to 120 columns) and on Ctrl+O
    let real = pg.counts.clone();
    let _ = writeln!(s, "resize_ms\t{}", ms(pg.sync(84, &v, "").unwrap()));
    v.all_open = true;
    let _ = writeln!(s, "all_open_ms\t{}", ms(pg.sync(cw, &v, "").unwrap()));
    let _ = writeln!(s, "all_open_total_rows\t{}", pg.total());
    v.all_open = false;
    pg.sync(cw, &v, "");
    // loading one page: read the range, fold, render
    let mut loads: Vec<Duration> = (0..pg.pages.len())
        .map(|p| {
            let t = Instant::now();
            let f = pg.fold_page(p);
            std::hint::black_box(pg.render(p, &f, &v));
            t.elapsed()
        })
        .collect();
    let _ = writeln!(
        s,
        "page_load_median_ms\t{}\npage_load_p90_ms\t{}\npage_load_max_ms\t{}",
        ms(pct(&mut loads, 0.5)),
        ms(pct(&mut loads, 0.9)),
        ms(pct(&mut loads, 1.0))
    );
    // search over the whole log: stream it, render each page to text, keep only the matches
    for w in words {
        v.q = if w.chars().count() >= 3 {
            w.to_string()
        } else {
            String::new()
        };
        let d = pg.sync(cw, &v, w).unwrap();
        let _ = writeln!(
            s,
            "search_{w}_ms\t{}\nsearch_{w}_hits\t{}\nsearch_{w}_hit_bytes\t{}",
            ms(d),
            pg.hits.len(),
            pg.hits.len() * std::mem::size_of::<(usize, usize, usize, usize)>()
        );
        // jumping to the first match, the one furthest from the end: its page and a screen either side
        if let Some(&(p, r, _, _)) = pg.hits.first() {
            let top = (pg.start_of(p) + r).saturating_sub(vh / 2);
            pg.resident.clear();
            let t = Instant::now();
            let n = pg.ensure(top.saturating_sub(vh), (top + 2 * vh).min(pg.total()), &v);
            let _ = writeln!(
                s,
                "search_{w}_jump_ms\t{}\nsearch_{w}_jump_pages\t{n}",
                ms(t.elapsed())
            );
        }
    }
    v.q.clear();
    pg.sync(cw, &v, "");
    // (b): estimates from bytes and from lines, corrected as pages load
    let bytes: Vec<u64> = pg
        .pages
        .iter()
        .map(|p| pg.offsets[p.end] - pg.offsets[p.first])
        .collect();
    let lines: Vec<u64> = pg.pages.iter().map(|p| (p.end - p.first) as u64).collect();
    for &m in margins {
        for (name, wt) in [("bytes", &bytes), ("lines", &lines)] {
            let (e, j, b, k, d) = thumb_jumps(&real, wt, vh, m);
            let _ = writeln!(
                s,
                "thumb_{name}_m{m}\topen_err_pct {e:.1} max_jump {j} jumps_over_1 {b} backwards {k} max_off_true {d}"
            );
        }
    }
    let exact: Vec<u64> = real.iter().map(|&c| c as u64).collect();
    let (e, j, b, k, d) = thumb_jumps(&real, &exact, vh, 1);
    let _ = writeln!(
        s,
        "thumb_exact\topen_err_pct {e:.1} max_jump {j} jumps_over_1 {b} backwards {k} max_off_true {d}"
    );
    // the whole-file mode, for comparison: fold everything, render everything, search it
    let t = Instant::now();
    let mut f = Fold::default();
    for l in std::fs::read_to_string(path)?.lines() {
        if let Ok(e) = serde_json::from_str::<Value>(l) {
            f.apply(&e);
        }
    }
    let all = crate::conversation(&f, cw, &v);
    let _ = writeln!(s, "whole_load_ms\t{}", ms(t.elapsed()));
    let same = all.len() == pg.total()
        && pg
            .rows(0, pg.total() - 1, &v)
            .1
            .iter()
            .zip(&all)
            .all(|(a, b)| crate::plain(a) == crate::plain(b));
    let _ = writeln!(s, "whole_rows\t{}\nrows_match_whole\t{same}", all.len());
    for w in words {
        v.q = if w.chars().count() >= 3 {
            w.to_string()
        } else {
            String::new()
        };
        let t = Instant::now();
        let all = crate::conversation(&f, cw, &v);
        let h = find_all(&all, w);
        let _ = writeln!(
            s,
            "whole_search_{w}_ms\t{}\nwhole_search_{w}_hits\t{}",
            ms(t.elapsed()),
            h.len()
        );
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{conversation, plain, selection_text};

    fn whole(path: &str, w: usize, v: &View) -> Vec<Row> {
        let mut f = Fold::default();
        for l in std::fs::read_to_string(path).unwrap().lines() {
            f.apply(&serde_json::from_str(l).unwrap());
        }
        conversation(&f, w, v)
    }
    fn text(rows: &[Row]) -> Vec<String> {
        rows.iter().map(plain).collect()
    }

    #[test]
    fn pages_join_into_the_rows_of_the_whole_file() {
        for path in [
            "fixtures/session.jsonl",
            "fixtures/idle.jsonl",
            "fixtures/large-median.jsonl",
        ] {
            for (page_lines, all_open, w) in [(8, false, 124), (8, true, 84), (64, false, 124)] {
                let v = View {
                    all_open,
                    ..Default::default()
                };
                let (mut pg, _) = Pager::open(path, page_lines).unwrap();
                pg.sync(w, &v, "");
                assert!(
                    pg.pages.len() > pg.turns.len() || page_lines == 64,
                    "{path}: turns are cut into pages"
                );
                let (base, rows) = pg.rows(0, pg.total() - 1, &v);
                assert_eq!(base, 0);
                assert_eq!(
                    text(&rows),
                    text(&whole(path, w, &v)),
                    "{path} at {page_lines} lines, open {all_open}, width {w}"
                );
            }
        }
    }

    #[test]
    fn a_selection_across_pages_copies_whole_and_in_order_after_its_pages_are_dropped() {
        let path = "fixtures/large-median.jsonl";
        let (w, vh, v) = (124, 10, View::default());
        let (mut pg, _) = Pager::open(path, 8).unwrap();
        pg.sync(w, &v, "");
        let all = whole(path, w, &v);
        // the drag starts in a page on screen, near the top of the session
        let a = (5, 3);
        pg.ensure(0, vh, &v);
        let pa = pg.page_at(a.0);
        assert!(pg.resident(pa));
        // auto-scroll carries it down several pages; the window moves with it, dropping where it began
        let b = (pg.start_of(pg.page_at(a.0) + 5) + 2, 40);
        assert!(
            pg.page_at(b.0) >= pa + 5,
            "the selection crosses page boundaries"
        );
        pg.ensure(b.0 + 1 - vh, b.0 + 1, &v);
        assert!(!pg.resident(pa), "the page the drag began in was dropped");
        for (x, y) in [(a, b), (b, a)] {
            let (base, rows) = pg.rows(x.0.min(y.0), x.0.max(y.0), &v);
            let got = selection_text(&rows, (x.0 - base, x.1), (y.0 - base, y.1));
            assert_eq!(got, selection_text(&all, x, y));
            assert!(got.lines().count() > 20);
        }
        // and scrolling up into pages not loaded when the drag began
        let top = (pg.start_of(1) + 1, 0);
        pg.ensure(top.0, top.0 + vh, &v);
        let (base, rows) = pg.rows(top.0, b.0, &v);
        assert_eq!(
            selection_text(&rows, (b.0 - base, b.1), (top.0 - base, top.1)),
            selection_text(&all, b, top)
        );
    }

    /// A session with a handoff mid-turn, its note, and a person's handoff whose size after
    /// arrives in the next turn: pages cut at every chance land on both sides of each band.
    fn handoff_fixture() -> String {
        let mut n = 0;
        let mut e = |kind: &str, aid: &str, payload: serde_json::Value| {
            n += 1;
            serde_json::json!({ "kind": kind, "session_id": "s", "ts": 1000 * n, "action_id": aid, "payload": payload }).to_string() + "\n"
        };
        let usage =
            |n: u64| serde_json::json!({ "tokens": { "input": n, "cache_read": 0, "output": 0 } });
        let text = |t: &str| serde_json::json!({ "text": t });
        let mut out = String::new();
        out += &e(
            "turn_started",
            "",
            serde_json::json!({ "input": [{ "type": "message", "content": [{ "type": "text", "text": "go" }], "source": "driver", "command_id": "c" }] }),
        );
        for (i, t) in ["one", "two", "three"].iter().enumerate() {
            out += &e(
                "assistant_message_started",
                &format!("b{i}"),
                serde_json::json!({}),
            );
            out += &e(
                "assistant_message_completed",
                &format!("b{i}"),
                text(&format!("before {t}")),
            );
        }
        out += &e("usage_recorded", "", usage(402_000));
        out += &e(
            "handoff_started",
            "",
            serde_json::json!({ "trigger": "auto" }),
        );
        out += &e("assistant_message_started", "n1", serde_json::json!({}));
        out += &e("assistant_message_completed", "n1", text("THE NOTE"));
        out += &e("usage_recorded", "", usage(403_000));
        out += &e(
            "handoff_completed",
            "",
            serde_json::json!({ "outcome": "completed", "tokens_before": 402_000, "note": ["n1"] }),
        );
        for (i, t) in ["four", "five"].iter().enumerate() {
            out += &e(
                "assistant_message_started",
                &format!("c{i}"),
                serde_json::json!({}),
            );
            out += &e(
                "assistant_message_completed",
                &format!("c{i}"),
                text(&format!("after {t}")),
            );
            if i == 0 {
                out += &e("usage_recorded", "", usage(32_000));
            }
        }
        out += &e(
            "turn_completed",
            "",
            serde_json::json!({ "outcome": "completed" }),
        );
        out += &e(
            "turn_started",
            "",
            serde_json::json!({ "input": [{ "type": "message", "content": [{ "type": "text", "text": "/handoff" }], "source": "driver", "command_id": "c" }] }),
        );
        out += &e(
            "handoff_started",
            "",
            serde_json::json!({ "trigger": "person" }),
        );
        out += &e(
            "handoff_completed",
            "",
            serde_json::json!({ "outcome": "completed", "tokens_before": 40_000, "note": [] }),
        );
        out += &e(
            "turn_completed",
            "",
            serde_json::json!({ "outcome": "completed" }),
        );
        out += &e(
            "turn_started",
            "",
            serde_json::json!({ "input": [{ "type": "message", "content": [{ "type": "text", "text": "next" }], "source": "driver", "command_id": "c" }] }),
        );
        out += &e("assistant_message_started", "d0", serde_json::json!({}));
        out += &e("assistant_message_completed", "d0", text("fresh"));
        out += &e("usage_recorded", "", usage(12_000));
        out += &e(
            "turn_completed",
            "",
            serde_json::json!({ "outcome": "completed" }),
        );
        out
    }

    #[test]
    fn handoff_bands_render_the_same_paged_as_whole() {
        let path = std::env::temp_dir().join(format!(
            "tui-prototype-handoff-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(&path, handoff_fixture()).unwrap();
        let path = path.to_str().unwrap().to_string();
        let v = View::default();
        let all = text(&whole(&path, 90, &v));
        assert!(
            all.iter().any(|l| l.contains("402k → 32k"))
                && all.iter().any(|l| l.contains("40k → 12k")),
            "{all:#?}"
        );
        assert!(!all.iter().any(|l| l.contains("THE NOTE")));
        for page_lines in [1, 2, 3, 64] {
            let (mut pg, _) = Pager::open(&path, page_lines).unwrap();
            if page_lines == 1 {
                assert!(
                    pg.pages.iter().any(|p| p.after_band),
                    "a page starts right after a band"
                );
            }
            pg.sync(90, &v, "");
            let (_, rows) = pg.rows(0, pg.total() - 1, &v);
            assert_eq!(text(&rows), all, "at {page_lines} lines a page");
        }
        let _ = std::fs::remove_file(&path);
        // the converted real session, where it is present (it is private, so never committed)
        let real = "fixtures/real.jsonl";
        if std::path::Path::new(real).exists() {
            let all = text(&whole(real, 124, &v));
            assert_eq!(all.iter().filter(|l| l.contains("⇄ handoff")).count(), 3);
            let bands: Vec<usize> = (0..all.len())
                .filter(|&i| all[i].contains("⇄ handoff"))
                .collect();
            assert!(
                bands.iter().all(|&i| !all[i].contains('…')),
                "every band has its size after"
            );
            for page_lines in [1, 8, 50, 64] {
                let (mut pg, _) = Pager::open(real, page_lines).unwrap();
                pg.sync(124, &v, "");
                let (_, rows) = pg.rows(0, pg.total() - 1, &v);
                let rows = text(&rows);
                // the rows around each band first, so a failure points at the handoff
                for &i in &bands {
                    let (a, b) = (i.saturating_sub(8), (i + 8).min(all.len()));
                    assert_eq!(
                        rows.get(a..b),
                        Some(&all[a..b]),
                        "around the band at row {i}, {page_lines} lines a page"
                    );
                }
                assert_eq!(rows, all, "{real} at {page_lines} lines a page");
            }
        }
    }

    #[test]
    fn search_over_pages_finds_what_the_whole_file_finds() {
        let path = "fixtures/large-median.jsonl";
        let v = View {
            q: "tool".into(),
            ..Default::default()
        };
        let (mut pg, _) = Pager::open(path, 8).unwrap();
        pg.sync(124, &v, "tool");
        let paged: Vec<_> = pg
            .hits
            .iter()
            .map(|&(p, r, c, n)| (pg.start_of(p) + r, c, n))
            .collect();
        assert_eq!(paged, find_all(&whole(path, 124, &v), "tool"));
        assert!(!paged.is_empty());
    }

    #[test]
    fn the_panel_folds_survive_the_streaming_pass() {
        let path = "fixtures/session.jsonl";
        let (_, sum) = Pager::open(path, 64).unwrap();
        let mut f = Fold::default();
        for l in std::fs::read_to_string(path).unwrap().lines() {
            f.apply(&serde_json::from_str(l).unwrap());
        }
        let panel = |f: &Fold| {
            crate::panel_rows(f, 0, "", 34, 0)
                .iter()
                .map(plain)
                .collect::<Vec<_>>()
        };
        assert_eq!(panel(&sum), panel(&f));
        assert_eq!(sum.pending.len(), f.pending.len());
        assert_eq!(sum.queue, f.queue);
    }

    #[test]
    fn exact_counts_never_move_the_thumb_more_than_a_cell_per_row() {
        let real = [30, 5, 60, 12, 40, 2, 80];
        let exact: Vec<u64> = real.iter().map(|&c| c as u64).collect();
        assert_eq!(thumb_jumps(&real, &exact, 20, 0), (0.0, 1, 0, 0, 0));
        // a bad estimate moves it further, and off where it belongs
        let flat = vec![1u64; real.len()];
        let (err, _, _, _, off) = thumb_jumps(&real, &flat, 20, 0);
        assert!(err.abs() > 10.0 && off > 0);
    }
}
