//! The day of a record a provider dates by the instant. Which day an instant falls on
//! depends on the household's zone, and the provider reports the same record again on
//! later syncs. A record keeps the day it was given unless its instant moved, so that a
//! change of zone can't move money between months (spec §8.7).

use coffer_domain::time::IanaZone;
use coffer_store::SourceRecord;
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

/// The instants behind a record's days, for a provider that reports instants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordInstants {
    /// When it posted; `None` while it hasn't.
    pub posted: Option<Timestamp>,
    pub transacted: Option<Timestamp>,
    /// A pending record is dated by when it was transacted, if that is known.
    pub pending: bool,
}

impl RecordInstants {
    /// The day it is dated and the day it was transacted, in `zone`. The first is `None`
    /// only for a record with neither instant.
    #[must_use]
    pub fn days_in(self, zone: &TimeZone) -> (Option<Date>, Option<Date>) {
        let day = |at: Timestamp| at.to_zoned(zone.clone()).date();
        let posted = self.posted.map(day);
        let transacted = self.transacted.map(day);
        let dated = if self.pending {
            transacted.or(posted)
        } else {
            posted
        };
        (dated, transacted)
    }
}

/// How a record was dated: its instants, and what the household's zone was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDating {
    pub instants: RecordInstants,
    /// The zone chosen in the settings, if any.
    pub chosen: Option<IanaZone>,
    /// The zone a record with no chosen zone was dated in.
    pub default_zone: TimeZone,
}

impl RecordDating {
    /// Whether the day `known`, an earlier report of this record, is dated is the day these
    /// instants fall on in the zone it was dated in. Then nothing happened to that day, and
    /// another day now would come only from the zone having changed.
    #[must_use]
    pub fn keeps_dated_day(&self, known: &SourceRecord) -> bool {
        let zone = known.dated_in.as_ref();
        self.instants.days_in(self.zone_or_default(zone)).0 == Some(known.fields.posted_on)
    }

    /// The same for the day it was transacted, which has an instant and a zone of its own:
    /// a pending record that posts keeps it.
    #[must_use]
    pub fn keeps_transacted_day(&self, known: &SourceRecord) -> bool {
        let zone = known.transacted_in.as_ref();
        self.instants.days_in(self.zone_or_default(zone)).1 == known.fields.transacted_on
    }

    fn zone_or_default<'a>(&'a self, zone: Option<&'a IanaZone>) -> &'a TimeZone {
        zone.map_or(&self.default_zone, IanaZone::zone)
    }
}

/// Whether a pending `source` missing from a window starting on `start` may simply be from
/// before the window. The provider is asked from midnight in the zone `zone_now`. A record
/// dated in another zone can be on the window's first days and still from before that
/// midnight, by at most a day after `start`. Its absence then says nothing.
#[must_use]
pub fn may_precede_window(source: &SourceRecord, start: Date, zone_now: Option<&IanaZone>) -> bool {
    source.dated_in.as_ref() != zone_now
        && source.fields.posted_on <= start.tomorrow().unwrap_or(start)
}
