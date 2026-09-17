//! Hours allocation rule:
//! - Exactly one repo has commits that day -> it gets the full daily total (8h).
//! - Multiple repos -> split the daily total by weight = (commit count +
//!   added lines + removed lines), with the remainder going to the first repo
//!   so the total always sums to the daily total.

/// Weight inputs for one repo.
#[derive(Debug, Clone)]
pub struct RepoWeight {
    pub repo: String,
    pub issue_key: String,
    pub commit_count: u64,
    pub added: u64,
    pub removed: u64,
}

impl RepoWeight {
    pub fn weight(&self) -> u64 {
        // commit count + diff churn. Guard against zero (repo with an empty-diff
        // commit) so it still gets a minimal share.
        let w = self.commit_count + self.added + self.removed;
        if w == 0 { 1 } else { w }
    }
}

/// Allocated seconds for one repo's worklog.
#[derive(Debug, Clone)]
pub struct Allocated {
    pub issue_key: String,
    pub repo: String,
    pub seconds: u64,
}

/// Compute per-issue seconds so the total equals `total_seconds`.
pub fn allocate(total_seconds: u64, repos: &[RepoWeight]) -> Vec<Allocated> {
    if repos.is_empty() {
        return vec![];
    }
    // Single repo: full total, no split.
    if repos.len() == 1 {
        return vec![Allocated {
            issue_key: repos[0].issue_key.clone(),
            repo: repos[0].repo.clone(),
            seconds: total_seconds,
        }];
    }

    let total_weight: u64 = repos.iter().map(|r| r.weight()).sum();
    let mut alloc = Vec::with_capacity(repos.len());
    let mut assigned = 0u64;
    for (i, r) in repos.iter().enumerate() {
        let secs = if i == repos.len() - 1 {
            // Last repo absorbs rounding remainder so the total is exact.
            total_seconds - assigned
        } else {
            let s = total_seconds * r.weight() / total_weight;
            assigned += s;
            s
        };
        alloc.push(Allocated { issue_key: r.issue_key.clone(), repo: r.repo.clone(), seconds: secs });
    }
    alloc
}

/// Total hours (as a display fraction) for the log filename.
pub fn total_hours(total_seconds: u64) -> f64 {
    total_seconds as f64 / 3600.0
}

/// Format the hours for a filename, e.g. 28800s -> "8", 14400s -> "4".
/// Returns an integer when whole, otherwise one decimal.
pub fn hours_label(total_seconds: u64) -> String {
    let h = total_hours(total_seconds);
    if (h - h.round()).abs() < 1e-9 {
        format!("{}", h.round() as i64)
    } else {
        format!("{:.1}", h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one() -> Vec<RepoWeight> {
        vec![RepoWeight { repo: "r1".into(), issue_key: "A-1".into(), commit_count: 3, added: 100, removed: 10 }]
    }

    #[test]
    fn single_repo_gets_full() {
        let a = allocate(28800, &one());
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].seconds, 28800);
    }

    #[test]
    fn multi_repo_splits_and_sums_to_total() {
        let repos = vec![
            RepoWeight { repo: "a".into(), issue_key: "A-1".into(), commit_count: 3, added: 100, removed: 10 }, // w=113
            RepoWeight { repo: "b".into(), issue_key: "B-1".into(), commit_count: 1, added: 10, removed: 2 },   // w=13
        ];
        let a = allocate(28800, &repos);
        let sum: u64 = a.iter().map(|x| x.seconds).sum();
        assert_eq!(sum, 28800);
        // a (heavier) gets more than b
        assert!(a[0].seconds > a[1].seconds);
    }

    #[test]
    fn empty_is_empty() {
        assert!(allocate(28800, &[]).is_empty());
    }

    #[test]
    fn zero_weight_repo_still_gets_share() {
        let repos = vec![
            RepoWeight { repo: "a".into(), issue_key: "A-1".into(), commit_count: 0, added: 0, removed: 0 },
            RepoWeight { repo: "b".into(), issue_key: "B-1".into(), commit_count: 2, added: 50, removed: 5 },
        ];
        let a = allocate(28800, &repos);
        let sum: u64 = a.iter().map(|x| x.seconds).sum();
        assert_eq!(sum, 28800);
    }

    #[test]
    fn hours_label_whole() {
        assert_eq!(hours_label(28800), "8");
        assert_eq!(hours_label(14400), "4");
    }

    #[test]
    fn hours_label_fractional() {
        assert_eq!(hours_label(9000), "2.5");
    }
}
