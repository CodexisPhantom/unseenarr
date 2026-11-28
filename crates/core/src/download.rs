//! The download state machine (docs/06-state-machines.md, "Download" and
//! "Retries").
//!
//! Every write of `downloads.state` except the initial grab insert goes
//! through [`transition`]: it is the single place that knows which moves are
//! allowed.

use std::collections::VecDeque;
use std::fmt;

/// A row in `downloads.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DownloadState {
    Queued,
    Downloading,
    Paused,
    Completed,
    Importing,
    ImportFailed,
    Imported,
    Failed,
    Removed,
}

impl DownloadState {
    /// The stored string, for example `"importFailed"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Importing => "importing",
            Self::ImportFailed => "importFailed",
            Self::Imported => "imported",
            Self::Failed => "failed",
            Self::Removed => "removed",
        }
    }

    /// Parses a stored value. `None` when it names no state.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "downloading" => Some(Self::Downloading),
            "paused" => Some(Self::Paused),
            "completed" => Some(Self::Completed),
            "importing" => Some(Self::Importing),
            "importFailed" => Some(Self::ImportFailed),
            "imported" => Some(Self::Imported),
            "failed" => Some(Self::Failed),
            "removed" => Some(Self::Removed),
            _ => None,
        }
    }

    /// Terminal states: `Imported`, `Failed` and `Removed` leave the machine.
    #[must_use]
    pub fn is_final(self) -> bool {
        matches!(self, Self::Imported | Self::Failed | Self::Removed)
    }

    /// The download client is the source of truth only before an import
    /// starts: `Queued`, `Downloading` and `Paused` are polled.
    #[must_use]
    pub fn is_polled(self) -> bool {
        matches!(self, Self::Queued | Self::Downloading | Self::Paused)
    }
}

impl fmt::Display for DownloadState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A refused move of the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidTransition {
    pub from: DownloadState,
    pub to: DownloadState,
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cannot move a download from {} to {}",
            self.from, self.to
        )
    }
}

impl std::error::Error for InvalidTransition {}

/// The direct moves out of `from`, exactly the diagram in
/// docs/06-state-machines.md.
fn edges(from: DownloadState) -> &'static [DownloadState] {
    use DownloadState as S;
    match from {
        S::Queued => &[S::Downloading, S::Failed, S::Removed],
        S::Downloading => &[S::Paused, S::Completed, S::Failed, S::Removed],
        S::Paused => &[S::Downloading],
        S::Completed => &[S::Importing],
        S::Importing => &[S::Imported, S::ImportFailed],
        S::ImportFailed => &[S::Importing],
        S::Imported | S::Failed | S::Removed => &[],
    }
}

/// The steps to take to get from `from` to `to`, ending with `to`.
/// `Ok(vec![])` when they are equal; `Err` when the machine has no path.
/// Walks the graph (BFS), so `Queued` to `Completed` yields
/// `[Downloading, Completed]` and `Paused` to `Failed` yields
/// `[Downloading, Failed]`.
pub fn transition(
    from: DownloadState,
    to: DownloadState,
) -> Result<Vec<DownloadState>, InvalidTransition> {
    if from == to {
        return Ok(Vec::new());
    }
    let mut previous: std::collections::HashMap<DownloadState, DownloadState> =
        std::collections::HashMap::new();
    let mut queue = VecDeque::from([from]);
    previous.insert(from, from);
    while let Some(current) = queue.pop_front() {
        for next in edges(current) {
            if previous.contains_key(next) {
                continue;
            }
            previous.insert(*next, current);
            if *next == to {
                let mut path = vec![to];
                let mut at = to;
                while previous[&at] != at {
                    at = previous[&at];
                    path.push(at);
                }
                path.pop();
                path.reverse();
                return Ok(path);
            }
            queue.push_back(*next);
        }
    }
    Err(InvalidTransition { from, to })
}

/// What one poll of one download client said about one download.
#[derive(Debug, Clone, PartialEq)]
pub enum Observation {
    /// The client has the item.
    Present {
        progress: f64,
        queued: bool,
        paused: bool,
        complete: bool,
        speed: i64,
        eta: Option<i64>,
        error: Option<String>,
    },
    /// The client answered but no longer has the item.
    Missing,
    /// The client did not answer.
    Unreachable,
}

/// What the poller remembers between polls of one download.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tracking {
    pub progress: f64,
    pub last_progress_at: i64,
    pub missing_polls: u32,
}

impl Tracking {
    /// First sight of a download: nothing missed yet.
    #[must_use]
    pub fn new(progress: f64, now: i64) -> Self {
        Self {
            progress,
            last_progress_at: now,
            missing_polls: 0,
        }
    }
}

/// The knobs of [`poll`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollSettings {
    /// Seconds without progress after which a downloading item fails.
    /// `0` turns the stall check off.
    pub stalled_after_seconds: i64,
    /// Consecutive polls missing from the client after which the item fails.
    pub fail_after_missing_polls: u32,
}

/// The state to store after one poll of one client.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub state: DownloadState,
    pub tracking: Tracking,
    pub error: Option<String>,
    pub progress: f64,
    pub speed: i64,
    pub eta: Option<i64>,
}

/// One poll of one client, from what the client said to the state to store.
///
/// 1. Unreachable: the state is unchanged; `last_progress_at` is `now` (time
///    while the client is unreachable does not count as stalled);
///    `missing_polls` is unchanged; progress comes from tracking, speed is 0,
///    eta is None, error is None.
/// 2. Missing: `missing_polls` goes up by one; `Failed` with the error "the
///    download client no longer has this item" when the new count reaches
///    `fail_after_missing_polls` (at least 1); otherwise the state is
///    unchanged. `last_progress_at` is `now`.
/// 3. Present with `error: Some(reason)`: the target is `Failed` with that
///    reason.
/// 4. Present and complete: the target is `Completed`.
/// 5. Present and paused: the target is `Paused`.
/// 6. Present otherwise: the target is `Downloading`. The stall check runs
///    only when the current state is `Downloading` and the target is
///    `Downloading`: when `stalled_after_seconds` is positive and the
///    progress did not move since `tracking` and more than the limit passed,
///    the target is `Failed` with `"no progress for {minutes} minutes"`.
///    Time while paused does not count, because a paused item targets
///    `Paused`, not `Downloading`.
/// 7. `last_progress_at` becomes `now` when the progress moved or the
///    observation is paused, complete, or an error; otherwise it is kept.
/// 8. The target is guarded with [`transition`]: one poll writes one step, so
///    a target that needs two steps stores the first one (a `Queued` row the
///    client reports complete goes to `Downloading` now and `Completed` on
///    the next poll). A refused target (a bug, or a row that is already
///    final) keeps the current state. Progress, speed and eta in the decision
///    always come from the observation, or from tracking, 0 and None for
///    Unreachable/Missing.
/// 9. `Decision.tracking.progress` is the observed progress on Present, and
///    is unchanged otherwise. A Present observation resets `missing_polls`.
#[must_use]
pub fn poll(
    state: DownloadState,
    tracking: &Tracking,
    observation: &Observation,
    settings: PollSettings,
    now: i64,
) -> Decision {
    match observation {
        Observation::Unreachable => Decision {
            state,
            tracking: Tracking {
                progress: tracking.progress,
                last_progress_at: now,
                missing_polls: tracking.missing_polls,
            },
            error: None,
            progress: tracking.progress,
            speed: 0,
            eta: None,
        },
        Observation::Missing => {
            let missing_polls = tracking.missing_polls.saturating_add(1);
            let fail_after = settings.fail_after_missing_polls.max(1);
            let (state, error) = if missing_polls >= fail_after {
                (
                    DownloadState::Failed,
                    Some("the download client no longer has this item".to_owned()),
                )
            } else {
                (state, None)
            };
            Decision {
                state,
                tracking: Tracking {
                    progress: tracking.progress,
                    last_progress_at: now,
                    missing_polls,
                },
                error,
                progress: tracking.progress,
                speed: 0,
                eta: None,
            }
        }
        Observation::Present {
            progress,
            queued,
            paused,
            complete,
            speed,
            eta,
            error,
        } => {
            let progress = progress.clamp(0.0, 1.0);
            let target = if error.is_some() {
                DownloadState::Failed
            } else if *complete {
                DownloadState::Completed
            } else if *paused {
                DownloadState::Paused
            } else if *queued {
                DownloadState::Queued
            } else {
                DownloadState::Downloading
            };
            let target = if target == DownloadState::Downloading
                && state == DownloadState::Downloading
                && settings.stalled_after_seconds > 0
                && progress == tracking.progress
                && now - tracking.last_progress_at > settings.stalled_after_seconds
            {
                DownloadState::Failed
            } else {
                target
            };
            let error = if target == DownloadState::Failed {
                if let Some(reason) = error {
                    Some(reason.clone())
                } else {
                    // The only clean observation that targets Failed is the
                    // stall above.
                    let elapsed = now - tracking.last_progress_at;
                    Some(format!("no progress for {} minutes", elapsed / 60))
                }
            } else {
                None
            };
            let state = match transition(state, target) {
                Ok(route) => route.first().copied().unwrap_or(state),
                Err(_) => state,
            };
            let last_progress_at =
                if progress != tracking.progress || *paused || *complete || error.is_some() {
                    now
                } else {
                    tracking.last_progress_at
                };
            Decision {
                state,
                tracking: Tracking {
                    progress,
                    last_progress_at,
                    missing_polls: 0,
                },
                error,
                progress,
                speed: *speed,
                eta: *eta,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> PollSettings {
        PollSettings {
            stalled_after_seconds: 360 * 60,
            fail_after_missing_polls: 2,
        }
    }

    fn tracking() -> Tracking {
        Tracking {
            progress: 0.4,
            last_progress_at: 1000,
            missing_polls: 0,
        }
    }

    fn present(progress: f64) -> Observation {
        Observation::Present {
            progress,
            queued: false,
            paused: false,
            complete: false,
            speed: 100,
            eta: Some(60),
            error: None,
        }
    }

    #[test]
    fn every_listed_edge_is_allowed() {
        use DownloadState as S;
        let edges = [
            (S::Queued, S::Downloading),
            (S::Queued, S::Failed),
            (S::Queued, S::Removed),
            (S::Downloading, S::Paused),
            (S::Downloading, S::Completed),
            (S::Downloading, S::Failed),
            (S::Downloading, S::Removed),
            (S::Paused, S::Downloading),
            (S::Completed, S::Importing),
            (S::Importing, S::Imported),
            (S::Importing, S::ImportFailed),
            (S::ImportFailed, S::Importing),
        ];
        for (from, to) in edges {
            assert!(transition(from, to).is_ok(), "{from} -> {to}");
        }
    }

    #[test]
    fn illegal_moves_are_refused() {
        use DownloadState as S;
        assert!(transition(S::Imported, S::Downloading).is_err());
        assert!(transition(S::Completed, S::Failed).is_err());
        assert!(transition(S::Failed, S::Queued).is_err());
        assert!(transition(S::Removed, S::Queued).is_err());
        assert!(transition(S::Importing, S::Downloading).is_err());
    }

    #[test]
    fn multi_step_paths_walk_the_graph() {
        use DownloadState as S;
        assert_eq!(
            transition(S::Queued, S::Completed),
            Ok(vec![S::Downloading, S::Completed])
        );
        assert_eq!(
            transition(S::Paused, S::Failed),
            Ok(vec![S::Downloading, S::Failed])
        );
        assert_eq!(transition(S::Queued, S::Queued), Ok(vec![]));
    }

    #[test]
    fn state_names_round_trip() {
        use DownloadState as S;
        let all = [
            S::Queued,
            S::Downloading,
            S::Paused,
            S::Completed,
            S::Importing,
            S::ImportFailed,
            S::Imported,
            S::Failed,
            S::Removed,
        ];
        for state in all {
            assert_eq!(S::parse(state.as_str()), Some(state));
            assert_eq!(state.to_string(), state.as_str());
        }
        assert_eq!(S::parse("bogus"), None);
        assert!(S::Imported.is_final() && S::Failed.is_final() && S::Removed.is_final());
        assert!(!S::Queued.is_final());
        assert!(S::Queued.is_polled() && S::Downloading.is_polled() && S::Paused.is_polled());
        assert!(!S::Completed.is_polled() && !S::Imported.is_polled());
    }

    #[test]
    fn queued_moves_to_downloading_on_progress() {
        let decision = poll(
            DownloadState::Queued,
            &Tracking::new(0.0, 0),
            &present(0.1),
            settings(),
            10,
        );
        assert_eq!(decision.state, DownloadState::Downloading);
        assert_eq!(decision.error, None);
    }

    #[test]
    fn queued_client_item_stays_queued() {
        let observation = Observation::Present {
            progress: 0.0,
            queued: true,
            paused: false,
            complete: false,
            speed: 0,
            eta: None,
            error: None,
        };
        let decision = poll(
            DownloadState::Queued,
            &Tracking::new(0.0, 0),
            &observation,
            settings(),
            10,
        );

        assert_eq!(decision.state, DownloadState::Queued);
    }

    #[test]
    fn a_complete_observation_steps_one_edge() {
        let done = Observation::Present {
            progress: 1.0,
            queued: false,
            paused: false,
            complete: true,
            speed: 0,
            eta: None,
            error: None,
        };
        let first = poll(
            DownloadState::Queued,
            &Tracking::new(0.0, 0),
            &done,
            settings(),
            10,
        );
        assert_eq!(first.state, DownloadState::Downloading);
        let second = poll(first.state, &first.tracking, &done, settings(), 20);
        assert_eq!(second.state, DownloadState::Completed);
    }

    #[test]
    fn missing_fails_on_the_second_poll_but_not_the_first() {
        let first = poll(
            DownloadState::Queued,
            &tracking(),
            &Observation::Missing,
            settings(),
            2000,
        );
        assert_eq!(first.state, DownloadState::Queued);
        assert_eq!(first.error, None);
        assert_eq!(first.tracking.missing_polls, 1);
        let second = poll(
            DownloadState::Queued,
            &first.tracking,
            &Observation::Missing,
            settings(),
            3000,
        );
        assert_eq!(second.state, DownloadState::Failed);
        assert!(second.error.as_deref().unwrap().contains("no longer"));
    }

    #[test]
    fn unreachable_keeps_the_state_and_resets_the_stall_clock() {
        let stale = Tracking {
            progress: 0.4,
            last_progress_at: 0,
            missing_polls: 1,
        };
        let decision = poll(
            DownloadState::Downloading,
            &stale,
            &Observation::Unreachable,
            settings(),
            100_000,
        );
        assert_eq!(decision.state, DownloadState::Downloading);
        assert_eq!(decision.error, None);
        assert_eq!(decision.tracking.last_progress_at, 100_000);
        assert_eq!(decision.tracking.missing_polls, 1);
        assert_eq!(decision.progress, 0.4);
        // The reset clock means the next poll with the same progress is not
        // stalled yet.
        let next = poll(
            DownloadState::Downloading,
            &decision.tracking,
            &present(0.4),
            settings(),
            100_001,
        );
        assert_eq!(next.state, DownloadState::Downloading);
    }

    #[test]
    fn stall_fails_past_the_limit_and_paused_time_does_not_count() {
        let stalled = Tracking {
            progress: 0.4,
            last_progress_at: 0,
            missing_polls: 0,
        };
        let failed = poll(
            DownloadState::Downloading,
            &stalled,
            &present(0.4),
            settings(),
            360 * 60 + 1,
        );
        assert_eq!(failed.state, DownloadState::Failed);
        assert!(failed.error.as_deref().unwrap().contains("no progress"));

        let not_yet = poll(
            DownloadState::Downloading,
            &stalled,
            &present(0.4),
            settings(),
            360 * 60,
        );
        assert_eq!(not_yet.state, DownloadState::Downloading);

        // A paused observation targets Paused, where the stall check runs.
        let paused = Observation::Present {
            progress: 0.4,
            queued: false,
            paused: true,
            complete: false,
            speed: 0,
            eta: None,
            error: None,
        };
        let kept = poll(
            DownloadState::Downloading,
            &stalled,
            &paused,
            settings(),
            10 * 360 * 60,
        );
        assert_eq!(kept.state, DownloadState::Paused);
        assert_eq!(kept.error, None);

        // Paused time does not count: back to Downloading with the same
        // progress is stalled only after a fresh window.
        let resumed = poll(
            DownloadState::Downloading,
            &kept.tracking,
            &present(0.4),
            settings(),
            kept.tracking.last_progress_at + 1,
        );
        assert_eq!(resumed.state, DownloadState::Downloading);
    }

    #[test]
    fn complete_reports_completed_and_client_error_fails() {
        let done = Observation::Present {
            progress: 1.0,
            queued: false,
            paused: false,
            complete: true,
            speed: 0,
            eta: None,
            error: None,
        };
        let decision = poll(
            DownloadState::Downloading,
            &tracking(),
            &done,
            settings(),
            2000,
        );
        assert_eq!(decision.state, DownloadState::Completed);

        let broken = Observation::Present {
            progress: 0.4,
            queued: false,
            paused: false,
            complete: false,
            speed: 0,
            eta: None,
            error: Some("boom".to_owned()),
        };
        let decision = poll(
            DownloadState::Queued,
            &tracking(),
            &broken,
            settings(),
            2000,
        );
        assert_eq!(decision.state, DownloadState::Failed);
        assert_eq!(decision.error.as_deref(), Some("boom"));
    }

    #[test]
    fn stalled_after_zero_turns_the_stall_check_off() {
        let settings = PollSettings {
            stalled_after_seconds: 0,
            fail_after_missing_polls: 2,
        };
        let stalled = Tracking {
            progress: 0.4,
            last_progress_at: 0,
            missing_polls: 0,
        };
        let decision = poll(
            DownloadState::Downloading,
            &stalled,
            &present(0.4),
            settings,
            10_000_000,
        );
        assert_eq!(decision.state, DownloadState::Downloading);
        assert_eq!(decision.error, None);
    }

    #[test]
    fn refused_targets_keep_the_current_state() {
        // `Completed` cannot go back to `Downloading`: a stale client answer
        // must not move it.
        let decision = poll(
            DownloadState::Completed,
            &tracking(),
            &present(0.5),
            settings(),
            2000,
        );
        assert_eq!(decision.state, DownloadState::Completed);
        assert_eq!(decision.progress, 0.5);
    }
}
