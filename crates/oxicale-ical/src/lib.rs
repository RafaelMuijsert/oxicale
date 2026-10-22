//! iCalendar parsing, validation, filtering and deterministic serialisation.

pub mod feed;
pub mod filter;
pub mod model;
pub mod parser;
pub mod recurrence;
pub mod validate;

pub use feed::{generate_feed, split_calendar, split_calendar_with_options, FetchedItem};
pub use filter::{
    CalendarFilter, ComponentFilter, FilterResult, ParamFilter, PropFilter, TextMatch,
};
pub use model::{Component, IcalTimeRange, Property};
pub use parser::{parse_calendar, IcalError, Limits};
pub use recurrence::{event_periods, item_matches_range, occurrence_count};
pub use validate::{validate_calendar_object, ValidatedCalendar};
