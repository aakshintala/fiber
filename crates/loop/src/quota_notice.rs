//! Session-owned quota crossings and their durable fold (`docs/tools.md`,
//! "The notice"; `docs/events.md`, "Usage and notices").

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use contract::events::{Event, QuotaInfo, QuotaNoticed, QuotaRearmed};

use crate::{Loop, prompt};

const CREDIT: &str = "credit";

/// Reads cached quota and `quota.notice_at` without starting a fetch or waiting.
pub type QuotaRead = Arc<dyn Fn() -> (Vec<QuotaInfo>, f64) + Send + Sync>;

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Fired(BTreeMap<(String, String, String), Option<u64>>);

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Due {
    pub(crate) events: Vec<Event>,
    pub(crate) lines: Vec<String>,
}

pub(crate) struct Notices {
    read: Option<QuotaRead>,
    pub(crate) fired: Fired,
}

impl Fired {
    pub(crate) fn fold(&mut self, event: &Event) {
        if let Event::QuotaNoticed(notice) = event {
            self.0.insert(
                (
                    notice.provider.clone(),
                    notice.credential.clone(),
                    notice.window.clone(),
                ),
                notice.resets_at,
            );
        } else if let Event::QuotaRearmed(notice) = event {
            self.0.remove(&(
                notice.provider.clone(),
                notice.credential.clone(),
                notice.window.clone(),
            ));
        }
    }

    pub(crate) fn judge(&self, quota: &[QuotaInfo], notice_at: f64, now_ms: u64) -> Due {
        let mut due = Due::default();
        let mut seen = BTreeSet::new();
        for info in quota {
            if info.unreachable.is_some() {
                continue;
            }
            let mut judge =
                |name: &str, percent: Option<f64>, reset: bool, resets_at: Option<u64>| {
                    let key = (
                        info.provider.clone(),
                        info.credential.clone(),
                        name.to_owned(),
                    );
                    // A duplicate cannot supersede even an unusable first reading.
                    if !seen.insert(key.clone()) {
                        return;
                    }
                    if reset {
                        return;
                    }
                    let Some(percent_used) = percent.filter(|p| !p.is_nan()) else {
                        return;
                    };
                    if resets_at.is_some_and(|at| at <= now_ms) {
                        return;
                    }
                    let fired = self
                        .0
                        .get(&key)
                        .is_some_and(|at| !at.is_some_and(|at| at <= now_ms));
                    if percent_used >= notice_at {
                        if !fired {
                            let notice = QuotaNoticed {
                                provider: key.0,
                                credential: key.1,
                                window: key.2,
                                percent_used,
                                resets_at,
                                notice_at,
                            };
                            due.lines.push(text(&notice));
                            due.events.push(Event::QuotaNoticed(notice));
                        }
                    } else if fired {
                        due.events.push(Event::QuotaRearmed(QuotaRearmed {
                            provider: key.0,
                            credential: key.1,
                            window: key.2,
                            percent_used,
                            notice_at,
                        }));
                    }
                };
            if let Some(windows) = &info.windows {
                for window in windows {
                    judge(
                        &window.name,
                        window.percent_used,
                        window.reset.is_some(),
                        window.resets_at,
                    );
                }
            }
            if let Some(credit) = &info.credit
                && let Some(limit) = credit.limit.filter(|limit| *limit > 0.0)
            {
                let percent = (100.0 * (limit - credit.remaining) / limit).clamp(0.0, 100.0);
                judge(CREDIT, Some(percent), false, None);
            }
        }
        due
    }
}

pub(crate) fn text(notice: &QuotaNoticed) -> String {
    let percent = notice.percent_used.round().to_string();
    let reset = notice
        .resets_at
        .map(|at| contract::clock::utc_time(at / 1000));
    let section = if reset.is_some() {
        "quota-notice-reset"
    } else {
        "quota-notice"
    };
    prompt::fill(
        prompt::message(section).trim_end(),
        &[
            ("provider", &notice.provider),
            ("credential", &notice.credential),
            ("window", &notice.window),
            ("percent", &percent),
            ("resets_at", reset.as_deref().unwrap_or_default()),
        ],
    )
}

impl Notices {
    pub(crate) fn new(fired: Fired) -> Self {
        Self { read: None, fired }
    }
}

impl Loop {
    /// Sets the cache-only quota reader. Crossings belong to this session;
    /// an unset reader gives no quota notices (`docs/tools.md`, "The notice").
    pub fn quota(mut self, read: QuotaRead) -> Self {
        self.quota_notices.read = Some(read);
        self
    }

    pub(crate) fn quota_due(&self) -> Due {
        let Some(read) = &self.quota_notices.read else {
            return Due::default();
        };
        let (quota, notice_at) = read();
        self.quota_notices.fired.judge(
            &quota,
            notice_at,
            contract::clock::wall_ms(self.log.clock().wall()),
        )
    }
}

#[cfg(test)]
#[path = "quota_notice_tests.rs"]
mod tests;
