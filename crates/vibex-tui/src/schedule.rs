//! When a message that is already written goes out.
//!
//! The Desktop lets the reader give a message a deadline instead of a turn:
//! the Composer's clock button schedules the draft it is holding, and a queued
//! message carries a deadline of its own. The message waits in the queue until
//! the clock reaches it, and the ordinary send path is what actually creates
//! the turn — a schedule never becomes a second way to send.
//!
//! The rules are the Desktop's on purpose. The same two shapes (`At` an
//! absolute local time, `After` a countdown that starts when the message is
//! submitted), the same one-year bound, the same refusal of a deadline that is
//! not in the future, and the same refusal of a local time the reader's own
//! clock cannot place unambiguously. What differs is the input: a terminal has
//! no calendar grid, so the deadline is typed as `YYYY-MM-DD HH:MM[:SS]` and
//! read back the same way.
//!
//! Everything here is pure. The value, the parse, the format and the form's own
//! editing state can be driven in a test without a terminal, which is what
//! keeps a client that cannot render a popover honest about the deadline it
//! accepts.

use chrono::{Local, NaiveDateTime, TimeZone as _};
use vibex_core::VibexSessionId;

use crate::app::ComposerTarget;
use crate::locale::Strings;

/// The longest countdown a schedule may carry: a year, so a reader cannot arm
/// a timer that outlives the process by accident.
pub const MAX_COUNTDOWN_SECONDS: u64 = 365 * 24 * 60 * 60;

/// The countdown a freshly opened editor offers, in seconds.
const DEFAULT_COUNTDOWN_SECONDS: u64 = 5 * 60;

/// How far from "now" a freshly opened absolute deadline starts.
const DEFAULT_LEAD_MS: i64 = 5 * 60 * 1_000;

/// The layouts the deadline field accepts, in the order they are tried. A
/// reader who leaves the seconds off means zero seconds.
const WHEN_LAYOUTS: [&str; 3] = ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S"];

/// When a message should be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageSchedule {
    /// An absolute local deadline, in milliseconds since the epoch.
    At(i64),
    /// A countdown, in seconds, that starts when the message is submitted.
    After(u64),
}

/// Why a schedule was refused, in the reader's own language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    /// An absolute deadline that has already passed.
    NotFuture,
    /// A countdown outside `1..=MAX_COUNTDOWN_SECONDS`, or with a minute or
    /// second field over 59.
    InvalidCountdown,
    /// A local time the reader's clock skips or repeats, where sending at the
    /// wrong instant is worse than asking again.
    AmbiguousLocalTime,
    /// A deadline that could not be read at all.
    UnreadableTime,
}

impl ScheduleError {
    pub fn message(self, strings: Strings) -> &'static str {
        match self {
            Self::NotFuture => strings.schedule_error_not_future(),
            Self::InvalidCountdown => strings.schedule_error_countdown(),
            Self::AmbiguousLocalTime => strings.schedule_error_ambiguous(),
            Self::UnreadableTime => strings.schedule_error_unreadable(),
        }
    }
}

impl MessageSchedule {
    /// The deadline this schedule names, given the moment the message is
    /// submitted.
    pub fn deadline(self, now_ms: i64) -> Result<i64, ScheduleError> {
        match self {
            Self::At(at_ms) if at_ms > now_ms => Ok(at_ms),
            Self::At(_) => Err(ScheduleError::NotFuture),
            Self::After(seconds) if (1..=MAX_COUNTDOWN_SECONDS).contains(&seconds) => now_ms
                .checked_add((seconds * 1_000) as i64)
                .ok_or(ScheduleError::InvalidCountdown),
            Self::After(_) => Err(ScheduleError::InvalidCountdown),
        }
    }
}

/// An absolute deadline as the reader's own clock reads it.
pub fn format_send_time(at_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at_ms)
        .map(|time| {
            time.with_timezone(&Local)
                .format(WHEN_LAYOUTS[0])
                .to_string()
        })
        .unwrap_or_default()
}

/// A countdown as `HH:MM:SS`, rounded up: a countdown that reads `00:00:01`
/// still has most of a second to run.
pub fn format_remaining(remaining_ms: i64) -> String {
    let seconds = remaining_ms.max(0).saturating_add(999) / 1_000;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// How long a scheduled message still waits, in the compact form a list row
/// can hold.
///
/// The seconds tick: a countdown that never moves reads as decoration, not as
/// the deadline the message is actually waiting for. Past a day the seconds
/// stop mattering and the days start to, so the shape changes with the distance
/// instead of growing an unreadable hour field. The Desktop's session rows use
/// exactly this shape.
pub fn format_schedule_countdown(remaining_ms: i64) -> String {
    let seconds = remaining_ms.max(0).saturating_add(999) / 1_000;
    if seconds < 24 * 60 * 60 {
        return format!(
            "{:02}:{:02}:{:02}",
            seconds / 3_600,
            seconds / 60 % 60,
            seconds % 60
        );
    }
    format!(
        "{}d {:02}:{:02}",
        seconds / (24 * 60 * 60),
        seconds / 3_600 % 24,
        seconds / 60 % 60
    )
}

/// How long a schedule still waits, for the surfaces that are not a list row:
/// a countdown keeps its seconds all the way out, an absolute deadline is
/// named.
pub fn schedule_summary(schedule: MessageSchedule) -> String {
    match schedule {
        MessageSchedule::At(at_ms) => format_send_time(at_ms),
        MessageSchedule::After(seconds) => format_remaining((seconds * 1_000) as i64),
    }
}

/// Read an hour / minute / second countdown.
pub fn parse_countdown(hours: &str, minutes: &str, seconds: &str) -> Result<u64, ScheduleError> {
    let parse = |value: &str| {
        value
            .trim()
            .parse::<u64>()
            .map_err(|_| ScheduleError::InvalidCountdown)
    };
    let (hours, minutes, seconds) = (parse(hours)?, parse(minutes)?, parse(seconds)?);
    if minutes >= 60 || seconds >= 60 {
        return Err(ScheduleError::InvalidCountdown);
    }
    hours
        .checked_mul(3_600)
        .and_then(|hours| hours.checked_add(minutes * 60 + seconds))
        .filter(|seconds| (1..=MAX_COUNTDOWN_SECONDS).contains(seconds))
        .ok_or(ScheduleError::InvalidCountdown)
}

/// Read an absolute local deadline the way the field writes it.
///
/// A time the reader's own clock skips (the hour a daylight-saving change
/// removes) or repeats (the hour it gives back) is refused rather than
/// silently resolved to one of the two instants.
pub fn parse_local_when(text: &str) -> Result<i64, ScheduleError> {
    let text = text.trim();
    let parsed = WHEN_LAYOUTS
        .iter()
        .find_map(|layout| NaiveDateTime::parse_from_str(text, layout).ok())
        .ok_or(ScheduleError::UnreadableTime)?;
    Local
        .from_local_datetime(&parsed)
        .single()
        .map(|time| time.timestamp_millis())
        .ok_or(ScheduleError::AmbiguousLocalTime)
}

/// The text an absolute deadline starts as when the editor opens on nothing.
pub fn default_when_text(now_ms: i64) -> String {
    format_send_time(now_ms.saturating_add(DEFAULT_LEAD_MS))
}

/// What a typed character may be in the deadline field: the digits and the
/// separators the layout uses, and nothing else. A stray letter would only be
/// refused a keystroke later, and the field is where that is worth saying.
pub fn when_field_accepts(character: char) -> bool {
    character.is_ascii_digit() || matches!(character, '-' | ':' | ' ' | 'T')
}

/// Which shape of deadline the editor is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleMode {
    At,
    After,
}

impl ScheduleMode {
    pub fn label(self, strings: Strings) -> &'static str {
        match self {
            Self::At => strings.schedule_mode_at(),
            Self::After => strings.schedule_mode_after(),
        }
    }
}

/// Which control of the editor owns the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScheduleField {
    /// The mode switch, so `Tab` can reach it without a pointer.
    Mode,
    When,
    Hours,
    Minutes,
    Seconds,
}

/// What the deadline the editor is holding is being given to.
///
/// The Desktop's clock button sits beside one Composer, so its value always
/// belongs to a draft: the message is written, and the hour says when it goes.
/// A client that is not the one holding a message has the same button to press
/// and no draft to put a value on — what it edits is the send the authority is
/// already keeping, and the value travels to whoever keeps the clock rather
/// than arming anything here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleEditTarget {
    /// The Composer's own draft.
    Draft(ComposerTarget),
    /// A send another client is holding, named the way the authority published
    /// it: the session it belongs to, and the queue entry under that session.
    Held {
        session_id: VibexSessionId,
        message_id: u64,
    },
}

/// The schedule editor one Composer target is showing.
///
/// The values live here rather than in the overlay's rendering, so a frame that
/// redraws the modal never rebuilds the fields the reader is typing into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageScheduleForm {
    /// What the value belongs to. A reader who navigates away while the overlay
    /// is open is applying the schedule to something that is gone — a draft the
    /// Composer has left, or a send the authority has since released — so the
    /// target travels with the form and is checked before the value is written.
    pub target: ScheduleEditTarget,
    pub mode: ScheduleMode,
    pub field: ScheduleField,
    pub when: String,
    pub hours: String,
    pub minutes: String,
    pub seconds: String,
    pub error: Option<ScheduleError>,
    /// Whether the target already carries a schedule, which is what makes
    /// clearing it something the reader can ask for.
    pub clearable: bool,
}

impl MessageScheduleForm {
    /// Open the editor on the value the target already holds, or on the
    /// deadline a reader who has not decided yet almost always means.
    pub fn new(target: ScheduleEditTarget, initial: Option<MessageSchedule>, now_ms: i64) -> Self {
        let default_hours = (DEFAULT_COUNTDOWN_SECONDS / 3_600).to_string();
        let default_minutes = (DEFAULT_COUNTDOWN_SECONDS / 60 % 60).to_string();
        let default_seconds = (DEFAULT_COUNTDOWN_SECONDS % 60).to_string();
        let (mode, when, hours, minutes, seconds) = match initial {
            Some(MessageSchedule::At(at_ms)) => (
                ScheduleMode::At,
                format_send_time(at_ms),
                default_hours,
                default_minutes,
                default_seconds,
            ),
            Some(MessageSchedule::After(seconds)) => (
                ScheduleMode::After,
                default_when_text(now_ms),
                (seconds / 3_600).to_string(),
                (seconds / 60 % 60).to_string(),
                (seconds % 60).to_string(),
            ),
            None => (
                ScheduleMode::At,
                default_when_text(now_ms),
                default_hours,
                default_minutes,
                default_seconds,
            ),
        };
        let field = match mode {
            ScheduleMode::At => ScheduleField::When,
            ScheduleMode::After => ScheduleField::Minutes,
        };
        Self {
            target,
            mode,
            field,
            when,
            hours,
            minutes,
            seconds,
            error: None,
            clearable: initial.is_some(),
        }
    }

    /// The fields the current mode actually shows, in the order `Tab` walks
    /// them.
    pub fn fields(&self) -> &'static [ScheduleField] {
        match self.mode {
            ScheduleMode::At => &[ScheduleField::Mode, ScheduleField::When],
            ScheduleMode::After => &[
                ScheduleField::Mode,
                ScheduleField::Hours,
                ScheduleField::Minutes,
                ScheduleField::Seconds,
            ],
        }
    }

    /// The buffer the given field edits, when it edits one.
    pub fn value(&self, field: ScheduleField) -> Option<&String> {
        match field {
            ScheduleField::When => Some(&self.when),
            ScheduleField::Hours => Some(&self.hours),
            ScheduleField::Minutes => Some(&self.minutes),
            ScheduleField::Seconds => Some(&self.seconds),
            ScheduleField::Mode => None,
        }
    }

    fn value_mut(&mut self, field: ScheduleField) -> Option<&mut String> {
        match field {
            ScheduleField::When => Some(&mut self.when),
            ScheduleField::Hours => Some(&mut self.hours),
            ScheduleField::Minutes => Some(&mut self.minutes),
            ScheduleField::Seconds => Some(&mut self.seconds),
            ScheduleField::Mode => None,
        }
    }

    /// The buffer the keyboard is on.
    pub fn editing_value_mut(&mut self) -> Option<&mut String> {
        let field = self.field;
        self.value_mut(field)
    }

    /// Move the keyboard along the fields this mode shows, wrapping.
    pub fn step_field(&mut self, delta: isize) {
        let fields = self.fields();
        let at = fields
            .iter()
            .position(|field| *field == self.field)
            .unwrap_or(0);
        let next = (at as isize + delta).rem_euclid(fields.len() as isize) as usize;
        self.field = fields[next];
    }

    /// Switch between an absolute deadline and a countdown, putting the
    /// keyboard where the edit usually starts in the mode arrived at.
    pub fn set_mode(&mut self, mode: ScheduleMode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.error = None;
        self.field = match mode {
            ScheduleMode::At => ScheduleField::When,
            ScheduleMode::After => ScheduleField::Minutes,
        };
    }

    /// Flip to the other shape. The values already typed are kept in both sets
    /// of buffers, so switching back and forth loses nothing.
    pub fn toggle_mode(&mut self) {
        self.set_mode(match self.mode {
            ScheduleMode::At => ScheduleMode::After,
            ScheduleMode::After => ScheduleMode::At,
        });
    }

    /// Add a character to the field the keyboard is on. The deadline field
    /// takes only what its layout is made of; a countdown field takes digits.
    pub fn push(&mut self, character: char) -> bool {
        let field = self.field;
        match field {
            ScheduleField::Mode => false,
            ScheduleField::When => {
                if !when_field_accepts(character) {
                    return false;
                }
                self.value_mut(field).is_some_and(|value| {
                    value.push(character);
                    true
                })
            }
            ScheduleField::Hours | ScheduleField::Minutes | ScheduleField::Seconds => {
                if !character.is_ascii_digit() {
                    return false;
                }
                self.value_mut(field).is_some_and(|value| {
                    // A countdown is three small numbers, not a document: a
                    // field that grew without bound would only be refused on
                    // submit, so it stops at the width of the largest value it
                    // could hold (a year of hours is four digits).
                    if value.len() >= 4 {
                        return false;
                    }
                    value.push(character);
                    true
                })
            }
        }
    }

    /// Take the last character back off the field the keyboard is on.
    pub fn backspace(&mut self) -> bool {
        self.editing_value_mut()
            .is_some_and(|value| value.pop().is_some())
    }

    /// Read the fields as a schedule, refusing anything the send path could not
    /// honour.
    pub fn validate(&self, now_ms: i64) -> Result<MessageSchedule, ScheduleError> {
        let schedule = match self.mode {
            ScheduleMode::At => MessageSchedule::At(parse_local_when(&self.when)?),
            ScheduleMode::After => {
                MessageSchedule::After(parse_countdown(&self.hours, &self.minutes, &self.seconds)?)
            }
        };
        schedule.deadline(now_ms).map(|_| schedule)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locale::Locale;
    use vibex_core::VibexSessionId;

    fn target() -> ScheduleEditTarget {
        ScheduleEditTarget::Draft(ComposerTarget::Session(
            VibexSessionId::parse("session_a").expect("valid session id"),
        ))
    }

    #[test]
    fn a_countdown_is_measured_from_the_moment_it_is_submitted() {
        assert_eq!(MessageSchedule::After(90).deadline(1_000), Ok(91_000));
        assert_eq!(MessageSchedule::At(5_000).deadline(1_000), Ok(5_000));
    }

    #[test]
    fn a_deadline_that_is_not_in_the_future_is_refused() {
        assert_eq!(
            MessageSchedule::At(1_000).deadline(1_000),
            Err(ScheduleError::NotFuture)
        );
        assert_eq!(
            MessageSchedule::At(999).deadline(1_000),
            Err(ScheduleError::NotFuture)
        );
    }

    #[test]
    fn a_countdown_outside_its_bounds_is_refused() {
        assert_eq!(
            MessageSchedule::After(0).deadline(0),
            Err(ScheduleError::InvalidCountdown)
        );
        assert_eq!(
            MessageSchedule::After(MAX_COUNTDOWN_SECONDS + 1).deadline(0),
            Err(ScheduleError::InvalidCountdown)
        );
        assert!(
            MessageSchedule::After(MAX_COUNTDOWN_SECONDS)
                .deadline(0)
                .is_ok()
        );
    }

    #[test]
    fn the_countdown_fields_are_read_as_one_duration() {
        assert_eq!(parse_countdown("1", "2", "3"), Ok(3_723));
        assert_eq!(parse_countdown("0", "0", "1"), Ok(1));
        assert_eq!(
            parse_countdown("", "5", "0"),
            Err(ScheduleError::InvalidCountdown)
        );
        assert_eq!(
            parse_countdown("0", "60", "0"),
            Err(ScheduleError::InvalidCountdown)
        );
        assert_eq!(
            parse_countdown("0", "0", "60"),
            Err(ScheduleError::InvalidCountdown)
        );
        assert_eq!(
            parse_countdown("0", "0", "0"),
            Err(ScheduleError::InvalidCountdown)
        );
        assert_eq!(
            parse_countdown("0", "0", "x"),
            Err(ScheduleError::InvalidCountdown)
        );
    }

    #[test]
    fn a_row_countdown_changes_shape_once_the_days_matter() {
        assert_eq!(format_schedule_countdown(0), "00:00:00");
        assert_eq!(format_schedule_countdown(-5_000), "00:00:00");
        assert_eq!(format_schedule_countdown(1), "00:00:01");
        assert_eq!(format_schedule_countdown(59_001), "00:01:00");
        assert_eq!(format_schedule_countdown(3_661_001), "01:01:02");
        assert_eq!(
            format_schedule_countdown(23 * 3_600_000 + 59 * 60_000 + 59_000),
            "23:59:59"
        );
        assert_eq!(format_schedule_countdown(24 * 3_600_000), "1d 00:00");
        assert_eq!(
            format_schedule_countdown(3 * 24 * 3_600_000 + 2 * 3_600_000 + 5 * 60_000),
            "3d 02:05"
        );
    }

    #[test]
    fn a_deadline_is_written_and_read_back_in_the_readers_own_clock() {
        let at_ms = 1_800_000_000_000;
        let text = format_send_time(at_ms);
        assert_eq!(text.len(), 19, "the field writes a full deadline: {text}");
        assert_eq!(parse_local_when(&text), Ok(at_ms));
        // The seconds are optional, because a reader typing a deadline rarely
        // has one in mind. A local offset is a whole number of minutes, so
        // dropping them lands on the same instant the clock would have shown.
        assert_eq!(
            parse_local_when(&text[..16]),
            Ok(at_ms - at_ms.rem_euclid(60_000))
        );
    }

    #[test]
    fn a_deadline_that_is_not_a_deadline_is_refused() {
        assert_eq!(
            parse_local_when("tomorrow"),
            Err(ScheduleError::UnreadableTime)
        );
        assert_eq!(
            parse_local_when("2026-13-01 00:00"),
            Err(ScheduleError::UnreadableTime)
        );
        assert_eq!(parse_local_when("  "), Err(ScheduleError::UnreadableTime));
        // A bare date is not a deadline: the reader would be scheduling
        // midnight without being told.
        assert_eq!(
            parse_local_when("2026-10-12"),
            Err(ScheduleError::UnreadableTime)
        );
    }

    #[test]
    fn the_editor_opens_on_a_deadline_it_can_actually_apply() {
        let now_ms = 1_800_000_000_000;
        let form = MessageScheduleForm::new(target(), None, now_ms);
        assert_eq!(form.mode, ScheduleMode::At);
        assert_eq!(
            form.validate(now_ms),
            Ok(MessageSchedule::At(now_ms + DEFAULT_LEAD_MS))
        );
        assert!(
            !form.clearable,
            "a target with no schedule has nothing to clear"
        );
    }

    #[test]
    fn the_editor_reopens_on_the_value_the_target_holds() {
        let now_ms = 1_800_000_000_000;
        let at =
            MessageScheduleForm::new(target(), Some(MessageSchedule::At(now_ms + 60_000)), now_ms);
        assert_eq!(at.mode, ScheduleMode::At);
        assert!(at.clearable);
        assert_eq!(
            at.validate(now_ms),
            Ok(MessageSchedule::At(now_ms + 60_000))
        );

        let after = MessageScheduleForm::new(target(), Some(MessageSchedule::After(3_600)), now_ms);
        assert_eq!(after.mode, ScheduleMode::After);
        assert_eq!(after.hours, "1");
        assert_eq!(after.minutes, "0");
        assert_eq!(after.seconds, "0");
        assert_eq!(after.validate(now_ms), Ok(MessageSchedule::After(3_600)));
    }

    #[test]
    fn tab_walks_only_the_fields_the_mode_shows() {
        let mut form = MessageScheduleForm::new(target(), None, 0);
        assert_eq!(form.field, ScheduleField::When);
        form.step_field(1);
        assert_eq!(form.field, ScheduleField::Mode);
        form.step_field(1);
        assert_eq!(form.field, ScheduleField::When, "the walk wraps");

        form.set_mode(ScheduleMode::After);
        assert_eq!(form.field, ScheduleField::Minutes);
        form.step_field(1);
        assert_eq!(form.field, ScheduleField::Seconds);
        form.step_field(1);
        assert_eq!(form.field, ScheduleField::Mode);
    }

    #[test]
    fn switching_shape_keeps_what_was_already_typed() {
        let mut form = MessageScheduleForm::new(target(), None, 0);
        form.when = "2026-10-12 08:30:00".to_string();
        form.set_mode(ScheduleMode::After);
        form.hours = "2".to_string();
        form.set_mode(ScheduleMode::At);
        assert_eq!(form.when, "2026-10-12 08:30:00");
        form.set_mode(ScheduleMode::After);
        assert_eq!(form.hours, "2");
    }

    #[test]
    fn the_deadline_field_takes_a_deadline_and_not_a_sentence() {
        let mut form = MessageScheduleForm::new(target(), None, 0);
        form.when.clear();
        for character in "2026-10-12 08:30:00".chars() {
            assert!(form.push(character), "{character} belongs in a deadline");
        }
        assert_eq!(form.when, "2026-10-12 08:30:00");
        assert!(!form.push('x'));
        assert_eq!(form.when, "2026-10-12 08:30:00");
    }

    #[test]
    fn a_countdown_field_stops_at_the_widest_value_it_could_hold() {
        let mut form = MessageScheduleForm::new(target(), None, 0);
        form.set_mode(ScheduleMode::After);
        form.minutes.clear();
        while form.push('9') {}
        assert_eq!(form.minutes, "9999");
        assert!(!form.push('9'));
        assert!(form.backspace());
        assert_eq!(form.minutes, "999");
    }

    #[test]
    fn the_editor_refuses_what_the_send_path_would_refuse() {
        let mut form = MessageScheduleForm::new(target(), None, 0);
        form.set_mode(ScheduleMode::After);
        form.minutes = "0".to_string();
        form.seconds = "0".to_string();
        assert_eq!(form.validate(0), Err(ScheduleError::InvalidCountdown));

        let at_ms = 1_800_000_000_000;
        let passed =
            MessageScheduleForm::new(target(), Some(MessageSchedule::At(at_ms)), at_ms - 1);
        assert_eq!(passed.validate(at_ms), Err(ScheduleError::NotFuture));
    }

    #[test]
    fn every_error_says_something_in_every_language() {
        for locale in [Locale::En, Locale::ZhCn, Locale::ZhTw] {
            let strings = Strings::for_locale(locale);
            for error in [
                ScheduleError::NotFuture,
                ScheduleError::InvalidCountdown,
                ScheduleError::AmbiguousLocalTime,
                ScheduleError::UnreadableTime,
            ] {
                assert!(!error.message(strings).is_empty());
            }
        }
    }

    #[test]
    fn a_local_time_the_clock_cannot_place_is_never_guessed() {
        // `Local` is the test host's zone, so the assertion is that a time the
        // clock repeats either resolves to exactly one instant or says so —
        // never that it silently picks one of the two.
        let parsed = parse_local_when("2026-03-08 02:30:00");
        match parsed {
            Ok(at_ms) => assert_eq!(format_send_time(at_ms).len(), 19),
            Err(error) => assert_eq!(error, ScheduleError::AmbiguousLocalTime),
        }
    }

    #[test]
    fn a_summary_names_the_deadline_the_surface_that_shows_it_needs() {
        assert_eq!(format_remaining(0), "00:00:00");
        assert_eq!(format_remaining(1), "00:00:01");
        assert_eq!(format_remaining(3_723_000), "01:02:03");
        let now_ms = 1_800_000_000_000;
        assert_eq!(schedule_summary(MessageSchedule::After(3_723)), "01:02:03");
        assert_eq!(
            schedule_summary(MessageSchedule::At(now_ms + 60_000)),
            format_send_time(now_ms + 60_000)
        );
    }
}
