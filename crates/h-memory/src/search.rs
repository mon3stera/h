use std::path::PathBuf;

use serde::Serialize;

use crate::{Entry, Scope};

#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    pub scope: Scope,
    pub id: String,
    pub title: String,
    pub summary: String,
    pub keywords: Vec<String>,
    pub path: PathBuf,
}

pub(crate) fn find(entries: Vec<Entry>, query: &str, limit: usize) -> Vec<Hit> {
    let terms = query
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    let mut matches = entries
        .into_iter()
        .filter_map(|entry| score(&entry, &terms).map(|score| (score, entry)))
        .collect::<Vec<_>>();

    matches.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| scope_rank(left.scope).cmp(&scope_rank(right.scope)))
            .then_with(|| left.id.cmp(&right.id))
    });

    matches
        .into_iter()
        .take(limit)
        .map(|(_, entry)| Hit {
            scope: entry.scope,
            id: entry.id,
            title: entry.title,
            summary: entry.summary,
            keywords: entry.keywords,
            path: entry.path,
        })
        .collect()
}

fn score(entry: &Entry, terms: &[String]) -> Option<usize> {
    let (id, title, summary, content) = (
        entry.id.to_lowercase(),
        entry.title.to_lowercase(),
        entry.summary.to_lowercase(),
        entry.content.to_lowercase(),
    );
    let keywords = entry
        .keywords
        .iter()
        .map(|keyword| keyword.to_lowercase())
        .collect::<Vec<_>>();
    let mut score = 0_usize;

    for term in terms {
        let term_score = usize::from(id.contains(term)) * 8
            + usize::from(title.contains(term)) * 6
            + usize::from(summary.contains(term)) * 4
            + keywords
                .iter()
                .filter(|keyword| keyword.contains(term))
                .count()
                * 3
            + usize::from(content.contains(term));

        if term_score == 0 {
            return None;
        }

        score += term_score;
    }

    Some(score)
}

const fn scope_rank(scope: Scope) -> usize {
    match scope {
        Scope::Project => 0,
        Scope::User => 1,
    }
}
