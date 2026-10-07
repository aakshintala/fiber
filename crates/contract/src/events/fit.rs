//! Whether a `reply`'s answer fits the interaction it names
//! (`docs/invocation.md`, "Replying"): the one check every asker applies,
//! an extension's `host.ask` and a running tool call alike.

use std::collections::HashSet;

use super::action::{Answer, FormAnswer, Interaction};
use crate::commands::{ReplyAnswer, SentFormAnswer};
use crate::shapes::{Choice, True};

impl Interaction {
    /// Fits `answer` to this interaction: `Some` with the [`Answer`] the
    /// resolution carries, or `None` when the answer's keys do not fit the
    /// request (`docs/invocation.md`, "Replying"). `declined` fits every
    /// kind; an approval or an offer's answer fits nothing.
    pub fn fit(&self, answer: &ReplyAnswer) -> Option<Answer> {
        if matches!(answer, ReplyAnswer::Declined { .. }) {
            // declined_fits_every_kind.
            return Some(Answer::Declined { declined: True });
        }
        match (self, answer) {
            (Interaction::Confirm { .. }, ReplyAnswer::Confirmed { confirmed }) => {
                Some(Answer::Confirmed {
                    confirmed: *confirmed,
                })
            }
            (Interaction::Select { options, .. }, ReplyAnswer::Labels { labels }) => {
                // select_takes_exactly_one_label.
                if labels.len() != 1 {
                    return None;
                }
                fit_labels(options, labels).then(|| Answer::Labels {
                    labels: labels.clone(),
                })
            }
            (Interaction::MultiSelect { options, .. }, ReplyAnswer::Labels { labels }) => {
                // multi_select_takes_distinct_offered_labels.
                if !distinct(labels) {
                    return None;
                }
                fit_labels(options, labels).then(|| Answer::Labels {
                    labels: labels.clone(),
                })
            }
            (Interaction::TextInput { .. }, ReplyAnswer::Text { text }) => {
                Some(Answer::Text { text: text.clone() })
            }
            (Interaction::Form { fields }, ReplyAnswer::Form { answers, note }) => {
                // form_answers_match_the_fields_in_order.
                if answers.len() != fields.len() {
                    return None;
                }
                let mut fitted = Vec::with_capacity(answers.len());
                for (field, answer) in fields.iter().zip(answers.iter()) {
                    match answer {
                        SentFormAnswer::Skipped { .. } => {
                            fitted.push(FormAnswer::Skipped { skipped: True });
                        }
                        SentFormAnswer::Answered { labels, text } => {
                            if !distinct(labels) || !fit_labels(&field.options, labels) {
                                return None;
                            }
                            let multi = field.multi_select.is_some_and(|multi| multi);
                            // at_most_one_label_unless_multi_select.
                            if !multi && labels.len() > 1 {
                                return None;
                            }
                            fitted.push(FormAnswer::Answered {
                                labels: labels.clone(),
                                text: text.clone(),
                            });
                        }
                    }
                }
                Some(Answer::Form {
                    answers: fitted,
                    note: note.clone(),
                })
            }
            _ => None,
        }
    }
}

/// Whether every label is one of the offered options.
fn fit_labels(options: &[Choice], labels: &[String]) -> bool {
    labels
        .iter()
        .all(|label| options.iter().any(|option| &option.label == label))
}

/// Whether no label repeats.
fn distinct(labels: &[String]) -> bool {
    let mut seen = HashSet::new();
    labels.iter().all(|label| seen.insert(label))
}

#[cfg(test)]
#[path = "fit_tests.rs"]
mod tests;
