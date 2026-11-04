use std::collections::BTreeMap;

use crate::limits::MAX_RECORDS;
use crate::report::Report;
use crate::types::{App, Id, Kind, Progress, State};

/// [`RecordStore`] settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreConfig {
    /// Maximum records per terminal. Defaults to 256. The spec requires
    /// terminals to support at least 64 (spec: *Limits*). Values below 1 are
    /// treated as 1.
    pub max_records: usize,
    /// Whether `idle` records are dropped on process exit and on a new shell
    /// prompt. The spec allows either (spec: *States*, Lifetime). Defaults to
    /// `false`.
    pub drop_idle_on_exit: bool,
}

impl Default for StoreConfig {
    fn default() -> Self {
        StoreConfig {
            max_records: MAX_RECORDS,
            drop_idle_on_exit: false,
        }
    }
}

/// One stored record: the keys from the last report that used this id
/// (spec: *Records and ids*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    report: Report,
    updated: u64,
}

impl Record {
    /// The report this record was set from.
    pub fn report(&self) -> &Report {
        &self.report
    }
    /// The record's state. Never [`State::Clear`].
    pub fn state(&self) -> State {
        self.report.state()
    }
    /// The record's own `kind`.
    pub fn kind(&self) -> Option<Kind> {
        self.report.kind()
    }
    /// The record's own `progress`. `None` means indeterminate.
    pub fn progress(&self) -> Option<Progress> {
        self.report.progress()
    }
    /// The record's own `app`, without inheritance. See
    /// [`RecordStore::effective_app`].
    pub fn app(&self) -> Option<&App> {
        self.report.app()
    }
    /// The record's `title`.
    pub fn title(&self) -> Option<&str> {
        self.report.title()
    }
    /// The record's `msg`.
    pub fn msg(&self) -> Option<&str> {
        self.report.msg()
    }
    /// Logical time of the last update. Larger means more recent. This is
    /// the order used for LRU eviction.
    pub fn updated(&self) -> u64 {
        self.updated
    }
}

/// Why a record was removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemoveReason {
    /// A `clear` report, or OSC 9;4 "remove".
    Cleared,
    /// Evicted as least recently updated, to make room under the record cap.
    Evicted,
    /// The attached process exited.
    ProcessExit,
    /// A new shell prompt began (OSC 133 A).
    Prompt,
    /// Full reset (RIS).
    Reset,
}

/// A change to the store, for terminals that raise events such as Rex's
/// `terminal.program_status_changed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The record was created or replaced. `None` is the root record.
    Set(Option<Id>),
    /// The record was removed.
    Removed {
        /// The record's id. `None` is the root record.
        id: Option<Id>,
        /// Why it was removed.
        reason: RemoveReason,
    },
}

/// An OSC 9;4 progress report, already parsed by the caller (spec:
/// *Relationship to other sequences*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Osc94 {
    /// `9;4;0`: remove progress. Removes the root record only.
    Remove,
    /// `9;4;1;N`: progress N. Maps to `working` with progress.
    Set(Progress),
    /// `9;4;2`: error. Maps to `error`.
    Error,
    /// `9;4;3`: indeterminate. Maps to `working` without progress.
    Indeterminate,
    /// `9;4;4;N`: paused. Maps to `working` with progress.
    Paused(Progress),
}

/// The receiver-side state for one terminal (spec: *Records and ids*,
/// *States*, *Limits*, *Relationship to other sequences*).
///
/// * Each report replaces its record completely.
/// * `clear` removes the addressed record and everything beneath it. `clear`
///   with no id removes every record.
/// * [`effective_app`](Self::effective_app) gives a record the `app` of its
///   nearest ancestor that has one.
/// * Past the record cap, the least recently updated record is evicted.
/// * OSC 9;4 is mapped to the root record until the first OSC 7501 report,
///   then ignored until the next RIS.
///
/// ```
/// use bellwether::{parse, RecordStore, State, Id};
///
/// let mut store = RecordStore::new();
/// store.apply(parse(b"state=working:app=deploy").unwrap());
/// store.apply(parse(b"state=blocked:kind=permission:id=eu-west").unwrap());
/// let eu = Id::new("eu-west").unwrap();
/// assert_eq!(store.get(Some(&eu)).unwrap().state(), State::Blocked);
/// assert_eq!(store.effective_app(Some(&eu)).unwrap().as_str(), "deploy");
///
/// store.on_prompt(); // working and blocked records are dropped
/// assert!(store.is_empty());
/// ```
#[derive(Debug, Clone, Default)]
pub struct RecordStore {
    records: BTreeMap<Option<Id>, Record>,
    clock: u64,
    config: StoreConfig,
    seen_7501: bool,
}

impl RecordStore {
    /// An empty store with the default [`StoreConfig`].
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty store with `config`.
    pub fn with_config(config: StoreConfig) -> Self {
        RecordStore {
            config,
            ..Self::default()
        }
    }

    /// Applies a parsed report. Returns what changed, in order.
    pub fn apply(&mut self, report: Report) -> Vec<Change> {
        self.seen_7501 = true;
        if report.state() == State::Clear {
            return match report.id() {
                None => self.remove_where(|_, _| true, RemoveReason::Cleared),
                Some(target) => {
                    let target = target.clone();
                    self.remove_where(
                        |id, _| {
                            id.as_ref()
                                .is_some_and(|id| *id == target || id.is_descendant_of(&target))
                        },
                        RemoveReason::Cleared,
                    )
                }
            };
        }
        self.set(report)
    }

    /// The attached process exited. Drops `working` and `blocked` records,
    /// and `idle` too if configured. `done` and `error` survive.
    pub fn on_process_exit(&mut self) -> Vec<Change> {
        self.drop_transient(RemoveReason::ProcessExit)
    }

    /// A new shell prompt began (OSC 133 A). Same rules as
    /// [`on_process_exit`](Self::on_process_exit).
    pub fn on_prompt(&mut self) -> Vec<Change> {
        self.drop_transient(RemoveReason::Prompt)
    }

    /// Full reset (RIS). Removes every record and turns OSC 9;4 mapping back
    /// on. Soft reset (DECSTR) and alternate-screen switches must *not* call
    /// this; they don't affect records.
    pub fn on_ris(&mut self) -> Vec<Change> {
        self.seen_7501 = false;
        self.remove_where(|_, _| true, RemoveReason::Reset)
    }

    /// Whether OSC 9;4 is still being mapped, i.e. no OSC 7501 report has been
    /// applied since the last RIS.
    pub fn osc_9_4_mapping_active(&self) -> bool {
        !self.seen_7501
    }

    /// Maps OSC 9;4 progress onto the root record. Does nothing once an OSC
    /// 7501 report has been applied, until the next [`on_ris`](Self::on_ris).
    pub fn apply_osc_9_4(&mut self, progress: Osc94) -> Vec<Change> {
        if self.seen_7501 {
            return Vec::new();
        }
        let builder = match progress {
            Osc94::Remove => {
                return self.remove_where(|id, _| id.is_none(), RemoveReason::Cleared);
            }
            Osc94::Set(p) | Osc94::Paused(p) => Report::builder(State::Working).progress(p),
            Osc94::Indeterminate => Report::builder(State::Working),
            Osc94::Error => Report::builder(State::Error),
        };
        self.set(builder.build_lossy())
    }

    /// The record at `id` (`None` = root).
    pub fn get(&self, id: Option<&Id>) -> Option<&Record> {
        self.records.get(&id.cloned())
    }

    /// The `app` of the record at `id`, or of its nearest ancestor that has
    /// one (spec: *Keys*). The record at `id` doesn't need to exist.
    pub fn effective_app(&self, id: Option<&Id>) -> Option<&App> {
        let mut key = id.cloned();
        loop {
            if let Some(app) = self.records.get(&key).and_then(Record::app) {
                return Some(app);
            }
            key = key?.parent();
        }
    }

    /// All records: the root first, then by id.
    pub fn iter(&self) -> impl Iterator<Item = (Option<&Id>, &Record)> {
        self.records.iter().map(|(k, v)| (k.as_ref(), v))
    }

    /// Number of records.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether there are no records.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn set(&mut self, report: Report) -> Vec<Change> {
        let mut changes = Vec::new();
        let key = report.id().cloned();
        let cap = self.config.max_records.max(1);
        if !self.records.contains_key(&key) && self.records.len() >= cap {
            let oldest = self
                .records
                .iter()
                .min_by_key(|(_, r)| r.updated)
                .map(|(k, _)| k.clone())
                .expect("store is non-empty at cap");
            self.records.remove(&oldest);
            changes.push(Change::Removed {
                id: oldest,
                reason: RemoveReason::Evicted,
            });
        }
        self.clock += 1;
        self.records.insert(
            key.clone(),
            Record {
                report,
                updated: self.clock,
            },
        );
        changes.push(Change::Set(key));
        changes
    }

    fn drop_transient(&mut self, reason: RemoveReason) -> Vec<Change> {
        let drop_idle = self.config.drop_idle_on_exit;
        self.remove_where(
            |_, r| match r.state() {
                State::Working | State::Blocked => true,
                State::Idle => drop_idle,
                _ => false,
            },
            reason,
        )
    }

    fn remove_where(
        &mut self,
        mut pred: impl FnMut(&Option<Id>, &Record) -> bool,
        reason: RemoveReason,
    ) -> Vec<Change> {
        let mut changes = Vec::new();
        self.records.retain(|id, r| {
            let remove = pred(id, r);
            if remove {
                changes.push(Change::Removed {
                    id: id.clone(),
                    reason,
                });
            }
            !remove
        });
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn apply(s: &mut RecordStore, body: &str) -> Vec<Change> {
        s.apply(parse(body.as_bytes()).unwrap())
    }
    fn id(s: &str) -> Id {
        Id::new(s).unwrap()
    }

    #[test]
    fn replaces_completely() {
        let mut s = RecordStore::new();
        apply(&mut s, "state=working:app=x:msg=SGk=");
        apply(&mut s, "state=done");
        let r = s.get(None).unwrap();
        assert_eq!(r.state(), State::Done);
        assert_eq!(r.app(), None);
        assert_eq!(r.msg(), None);
    }

    #[test]
    fn subtree_clear() {
        let mut s = RecordStore::new();
        for i in ["a", "a/b", "a/b/c", "ab", "z"] {
            apply(&mut s, &format!("state=working:id={i}"));
        }
        apply(&mut s, "state=idle");
        let ch = apply(&mut s, "state=clear:id=a");
        assert_eq!(ch.len(), 3);
        let left: Vec<_> = s.iter().map(|(k, _)| k.map(Id::as_str)).collect();
        assert_eq!(left, [None, Some("ab"), Some("z")]);
        // Clearing an id that doesn't exist is fine.
        assert!(apply(&mut s, "state=clear:id=nope").is_empty());
        apply(&mut s, "state=clear");
        assert!(s.is_empty());
    }

    #[test]
    fn app_inheritance() {
        let mut s = RecordStore::new();
        apply(&mut s, "state=working:app=deploy");
        apply(&mut s, "state=working:id=r/eu:app=eu");
        apply(&mut s, "state=working:id=r/eu/a");
        apply(&mut s, "state=working:id=r/us");
        let app = |i: &str| s.effective_app(Some(&id(i))).map(App::as_str);
        assert_eq!(app("r/eu/a"), Some("eu"));
        assert_eq!(app("r/us"), Some("deploy"));
        assert_eq!(app("r/missing/deep"), Some("deploy"));
        assert_eq!(s.effective_app(None).map(App::as_str), Some("deploy"));
    }

    #[test]
    fn lifetime_rules() {
        let mut s = RecordStore::new();
        for (i, st) in ["idle", "working", "done", "blocked", "error"]
            .iter()
            .enumerate()
        {
            apply(&mut s, &format!("state={st}:id=r{i}"));
        }
        let ch = s.on_prompt();
        assert_eq!(ch.len(), 2);
        let states: Vec<_> = s.iter().map(|(_, r)| r.state()).collect();
        assert_eq!(states, [State::Idle, State::Done, State::Error]);

        let mut s = RecordStore::with_config(StoreConfig {
            drop_idle_on_exit: true,
            ..Default::default()
        });
        apply(&mut s, "state=idle");
        apply(&mut s, "state=done:id=x");
        s.on_process_exit();
        assert_eq!(s.len(), 1);
        assert!(s.get(Some(&id("x"))).is_some());
    }

    #[test]
    fn lru_eviction() {
        let mut s = RecordStore::with_config(StoreConfig {
            max_records: 3,
            ..Default::default()
        });
        apply(&mut s, "state=working:id=a");
        apply(&mut s, "state=working:id=b");
        apply(&mut s, "state=working:id=c");
        apply(&mut s, "state=done:id=a"); // a becomes most recent; b is now LRU
        let ch = apply(&mut s, "state=working:id=d");
        assert_eq!(
            ch,
            [
                Change::Removed {
                    id: Some(id("b")),
                    reason: RemoveReason::Evicted
                },
                Change::Set(Some(id("d")))
            ]
        );
        // Replacing an existing record never evicts.
        assert_eq!(apply(&mut s, "state=done:id=c").len(), 1);
    }

    #[test]
    fn osc_9_4_mapping_stops_after_7501_until_ris() {
        let mut s = RecordStore::new();
        apply(&mut s, "state=idle:id=child");
        s.on_ris();
        assert!(s.osc_9_4_mapping_active());
        s.apply_osc_9_4(Osc94::Set(Progress::new(30).unwrap()));
        assert_eq!(s.get(None).unwrap().progress().map(Progress::get), Some(30));
        s.apply_osc_9_4(Osc94::Remove);
        assert!(s.get(None).is_none());

        apply(&mut s, "state=blocked:kind=auth");
        assert!(!s.osc_9_4_mapping_active());
        assert!(s.apply_osc_9_4(Osc94::Error).is_empty());
        assert_eq!(s.get(None).unwrap().kind(), Some(Kind::Auth));

        let ch = s.on_ris();
        assert_eq!(
            ch,
            [Change::Removed {
                id: None,
                reason: RemoveReason::Reset
            }]
        );
        assert!(s.osc_9_4_mapping_active());
        s.apply_osc_9_4(Osc94::Indeterminate);
        assert_eq!(s.get(None).unwrap().state(), State::Working);
    }

    #[test]
    fn clear_counts_as_7501_report() {
        let mut s = RecordStore::new();
        apply(&mut s, "state=clear");
        assert!(!s.osc_9_4_mapping_active());
    }
}
