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
    /// Whether `known`, an earlier report of this record, has the days these instants
    /// fall on in the zone `known` was dated in: then nothing happened to the record, and
    /// any other day now would come only from the zone having changed.
    #[must_use]
    pub fn matches(&self, known: &SourceRecord) -> bool {
        let zone = known
            .dated_in
            .as_ref()
            .map_or(&self.default_zone, IanaZone::zone);
        let (dated, transacted) = self.instants.days_in(zone);
        dated == Some(known.fields.posted_on) && transacted == known.fields.transacted_on
    }
}
