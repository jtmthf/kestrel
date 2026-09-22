//! Five fields of numbers, `*`, ranges, lists and steps, in the spirit of ADR-0012: no names, no
//! `?`, `L` or `W`, and never a day of the month alongside a day of the week, whose union
//! classic cron takes where a reader expects an intersection.

use std::fmt;
use std::ops::RangeInclusive;

use anyhow::{Context as _, Result, anyhow, bail};
use jiff::civil::{Date, DateTime};
use jiff::tz::{AmbiguousOffset, TimeZone};
use jiff::{SignedDuration, Timestamp, ToSpan as _};

const MINUTES_A_DAY: i64 = 24 * 60;

/// Far enough ahead to reach the next 29 February, the rarest day an expression can name.
const HORIZON_DAYS: i64 = 8 * 366 + 1;

#[derive(Debug, Clone)]
pub struct Cron {
    expression: String,
    zone_name: String,
    zone: TimeZone,
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
}

impl PartialEq for Cron {
    fn eq(&self, other: &Self) -> bool {
        self.expression == other.expression && self.zone_name == other.zone_name
    }
}

impl Eq for Cron {}

impl fmt::Display for Cron {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.expression)
    }
}

impl Cron {
    pub fn new(expression: &str, zone_name: &str) -> Result<Self> {
        let fields = expression.split_whitespace().collect::<Vec<_>>();
        let [minutes, hours, days, months, weekdays] = fields[..] else {
            bail!(
                "a cron expression has five fields, a minute, an hour, a day of the month, a \
                 month and a day of the week, and {expression:?} has {}",
                fields.len()
            );
        };
        let zone = TimeZone::get(zone_name)
            .with_context(|| format!("no time zone named {zone_name:?}"))?;

        let cron = Self {
            expression: fields.join(" "),
            zone_name: zone.iana_name().unwrap_or(zone_name).to_owned(),
            minutes: field(minutes, "minute", 0..=59)?,
            hours: field(hours, "hour", 0..=23)?,
            days: field(days, "day of the month", 1..=31)?,
            months: field(months, "month", 1..=12)?,
            weekdays: field(weekdays, "day of the week", 0..=6)?,
            zone,
        };
        if cron.days != all(1..=31) && cron.weekdays != all(0..=6) {
            bail!(
                "a cron expression restricts the day of the month or the day of the week, not both"
            );
        }
        if !(1..=12)
            .filter(|month| has(cron.months, *month))
            .any(|month| (1..=longest(month)).any(|day| has(cron.days, day)))
        {
            bail!(
                "the cron expression {} names no day that exists",
                cron.expression
            );
        }

        Ok(cron)
    }

    pub fn zone(&self) -> &str {
        &self.zone_name
    }

    /// A time a spring-forward gap skips elapses when the clocks jump, together with any other
    /// the same jump skips, and one a fall-back fold repeats elapses on its first pass.
    pub fn after(&self, at: Timestamp) -> Result<Timestamp> {
        let civil = at.to_zoned(self.zone.clone()).datetime();
        let from = civil.date();

        for day in 0..HORIZON_DAYS {
            let date = from.checked_add(day.days())?;
            if !self.falls_on(date) {
                continue;
            }
            for (hour, minute) in self.times() {
                let named = date.at(hour, minute, 0, 0);
                if named <= civil {
                    continue;
                }
                let due = self.instant(named)?;
                if due > at {
                    return Ok(due);
                }
            }
        }

        Err(anyhow!(
            "the cron expression {} names no time in the next eight years",
            self.expression
        ))
    }

    /// Counted on the clock, so a spring-forward jump can bring one pair closer once a year, and
    /// spacing across days is never counted as less than a day; neither can exhaust a budget.
    pub fn fastest(&self) -> SignedDuration {
        let times = self
            .times()
            .map(|(hour, minute)| i64::from(hour) * 60 + i64::from(minute))
            .collect::<Vec<_>>();
        let within = times.windows(2).map(|pair| pair[1] - pair[0]);
        let across = match (times.first(), times.last()) {
            (Some(first), Some(last)) if self.names_consecutive_days() => {
                first + MINUTES_A_DAY - last
            }
            _ => MINUTES_A_DAY,
        };

        SignedDuration::from_mins(within.chain([across]).min().unwrap_or(MINUTES_A_DAY))
    }

    fn falls_on(&self, date: Date) -> bool {
        has(self.months, date.month())
            && has(self.days, date.day())
            && has(self.weekdays, date.weekday().to_sunday_zero_offset())
    }

    fn times(&self) -> impl Iterator<Item = (i8, i8)> + '_ {
        (0..=23)
            .filter(|hour| has(self.hours, *hour))
            .flat_map(|hour| {
                (0..=59)
                    .filter(|minute| has(self.minutes, *minute))
                    .map(move |minute| (hour, minute))
            })
    }

    fn names_consecutive_days(&self) -> bool {
        if self.weekdays != all(0..=6) {
            return (0..=6).any(|day| has(self.weekdays, day) && has(self.weekdays, (day + 1) % 7));
        }

        (1..=12i8)
            .filter(|month| has(self.months, *month))
            .any(|month| {
                let next = month % 12 + 1;
                let within =
                    (1..longest(month)).any(|day| has(self.days, day) && has(self.days, day + 1));
                let into_next = has(self.days, 1)
                    && has(self.months, next)
                    && lengths(month).any(|last| has(self.days, last));
                within || into_next
            })
    }

    fn instant(&self, named: DateTime) -> Result<Timestamp> {
        let ambiguous = self.zone.to_ambiguous_timestamp(named);
        match ambiguous.offset() {
            AmbiguousOffset::Gap { after, .. } => {
                let before_the_jump = after.to_timestamp(named)?;
                Ok(self
                    .zone
                    .following(before_the_jump)
                    .next()
                    .context("a gap in a time zone ends in a transition")?
                    .timestamp())
            }
            AmbiguousOffset::Unambiguous { .. } | AmbiguousOffset::Fold { .. } => {
                Ok(ambiguous.earlier()?)
            }
        }
    }
}

fn field(text: &str, name: &str, range: RangeInclusive<i8>) -> Result<u64> {
    let mut mask = 0;
    for item in text.split(',') {
        let (span, step) = match item.split_once('/') {
            Some((span, step)) => {
                let step = step
                    .parse::<i8>()
                    .ok()
                    .filter(|step| (1..=*range.end()).contains(step))
                    .with_context(|| {
                        format!(
                            "the step in the {name} field {text:?} is not a number from 1 to {}",
                            range.end()
                        )
                    })?;
                (span, Some(step))
            }
            None => (item, None),
        };
        let span = match span.split_once('-') {
            _ if span == "*" => range.clone(),
            Some((start, end)) => {
                let (start, end) = (value(start, name, &range)?, value(end, name, &range)?);
                if start > end {
                    bail!("the range {span} in the {name} field runs backwards");
                }
                start..=end
            }
            None if step.is_some() => {
                bail!("a step in the {name} field counts over `*` or a range, not {span}")
            }
            None => {
                let at = value(span, name, &range)?;
                at..=at
            }
        };
        let step = usize::try_from(step.unwrap_or(1))?;
        for at in span.step_by(step) {
            mask |= 1 << at;
        }
    }

    Ok(mask)
}

fn value(text: &str, name: &str, range: &RangeInclusive<i8>) -> Result<i8> {
    text.parse::<i8>()
        .ok()
        .filter(|at| range.contains(at))
        .with_context(|| {
            format!(
                "the {name} field takes numbers from {} to {}, not {text:?}",
                range.start(),
                range.end()
            )
        })
}

fn all(range: RangeInclusive<i8>) -> u64 {
    range.fold(0, |mask, at| mask | 1 << at)
}

fn has(mask: u64, at: i8) -> bool {
    mask & 1 << at != 0
}

fn longest(month: i8) -> i8 {
    lengths(month).max().unwrap_or(31)
}

fn lengths(month: i8) -> impl Iterator<Item = i8> {
    let lengths: &[i8] = match month {
        2 => &[28, 29],
        4 | 6 | 9 | 11 => &[30],
        _ => &[31],
    };
    lengths.iter().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().expect("the timestamp should parse")
    }

    fn cron(expression: &str, zone: &str) -> Cron {
        Cron::new(expression, zone).expect("the cron expression should parse")
    }

    fn refusal(expression: &str) -> String {
        format!(
            "{:#}",
            Cron::new(expression, "UTC").expect_err("the cron expression should be refused")
        )
    }

    fn elapsings(cron: &Cron, from: &str, count: usize) -> Vec<Timestamp> {
        std::iter::successors(Some(at(from)), |due| {
            Some(cron.after(*due).expect("the expression should elapse"))
        })
        .skip(1)
        .take(count)
        .collect()
    }

    #[test]
    fn a_weekday_morning_skips_the_weekend() {
        let triage = cron("0 9 * * 1-5", "America/New_York");

        assert_eq!(
            elapsings(&triage, "2026-09-18T14:00:00Z", 2),
            [at("2026-09-21T13:00:00Z"), at("2026-09-22T13:00:00Z")],
            "a Friday afternoon is next due Monday at nine, New York time"
        );
    }

    #[test]
    fn the_next_elapsing_is_later_than_the_moment_asked_about() {
        let hourly = cron("0 * * * *", "UTC");

        assert_eq!(
            hourly.after(at("2026-09-21T10:00:00Z")).unwrap(),
            at("2026-09-21T11:00:00Z")
        );
        assert_eq!(
            hourly.after(at("2026-09-21T10:59:59.9Z")).unwrap(),
            at("2026-09-21T11:00:00Z")
        );
    }

    #[test]
    fn a_time_the_clocks_spring_past_elapses_when_they_jump() {
        let nightly = cron("30 2 * * *", "America/New_York");

        assert_eq!(
            elapsings(&nightly, "2026-03-07T12:00:00Z", 3),
            [
                at("2026-03-08T07:00:00Z"),
                at("2026-03-09T06:30:00Z"),
                at("2026-03-10T06:30:00Z"),
            ],
            "the eighth has no 02:30, so it elapses at 03:00 EDT"
        );
    }

    #[test]
    fn a_time_the_clocks_fall_back_over_elapses_once() {
        let nightly = cron("30 1 * * *", "America/New_York");

        assert_eq!(
            elapsings(&nightly, "2026-10-31T12:00:00Z", 2),
            [at("2026-11-01T05:30:00Z"), at("2026-11-02T06:30:00Z")],
            "01:30 happens twice on the first and elapses on the first pass"
        );
    }

    #[test]
    fn times_skipped_by_one_jump_elapse_together() {
        let often = cron("*/20 2 * * *", "America/New_York");

        assert_eq!(
            elapsings(&often, "2026-03-08T06:00:00Z", 2),
            [at("2026-03-08T07:00:00Z"), at("2026-03-09T06:00:00Z")]
        );
    }

    #[test]
    fn the_rarest_day_is_still_found() {
        let leap = cron("0 0 29 2 *", "UTC");

        assert_eq!(
            leap.after(at("2026-09-21T00:00:00Z")).unwrap(),
            at("2028-02-29T00:00:00Z")
        );
    }

    #[test]
    fn lists_ranges_and_steps_combine() {
        let mixed = cron("0,30 8-10/2 * * *", "UTC");

        assert_eq!(
            elapsings(&mixed, "2026-09-21T00:00:00Z", 5),
            [
                at("2026-09-21T08:00:00Z"),
                at("2026-09-21T08:30:00Z"),
                at("2026-09-21T10:00:00Z"),
                at("2026-09-21T10:30:00Z"),
                at("2026-09-22T08:00:00Z"),
            ]
        );
    }

    #[test]
    fn the_fastest_spacing_is_the_shortest_gap_between_named_times() {
        for (expression, fastest) in [
            ("*/10 * * * *", SignedDuration::from_mins(10)),
            ("0 9 * * 1-5", SignedDuration::from_hours(24)),
            ("0 9 * * 1,3", SignedDuration::from_hours(24)),
            ("0,50 9 * * *", SignedDuration::from_mins(50)),
            ("59 23 * * *", SignedDuration::from_hours(24)),
            ("0,59 0,23 * * *", SignedDuration::from_mins(1)),
            ("0,59 0,23 * * 1", SignedDuration::from_mins(59)),
            ("0,59 0,23 31 * *", SignedDuration::from_mins(59)),
            ("0,59 0,23 1,31 1,2 *", SignedDuration::from_mins(1)),
            ("0,59 0,23 1,31 2,3 *", SignedDuration::from_mins(59)),
            ("0,59 0,23 1,29 2,3 *", SignedDuration::from_mins(1)),
        ] {
            assert_eq!(cron(expression, "UTC").fastest(), fastest, "{expression}");
        }
    }

    #[test]
    fn an_expression_prints_back_as_it_was_written() {
        let spaced = cron(" 0  9 * *   1-5 ", "america/new_york");

        assert_eq!(spaced.to_string(), "0 9 * * 1-5");
        assert_eq!(spaced.zone(), "America/New_York");
        assert_eq!(spaced, cron("0 9 * * 1-5", "America/New_York"));
    }

    #[test]
    fn what_the_dialect_does_not_say_is_refused() {
        for (expression, because) in [
            ("0 9 * *", "has five fields"),
            ("0 9 * * 1-5 2026", "has five fields"),
            ("60 * * * *", "the minute field takes numbers from 0 to 59"),
            ("0 24 * * *", "the hour field takes numbers from 0 to 23"),
            (
                "0 0 0 * *",
                "the day of the month field takes numbers from 1 to 31",
            ),
            ("0 0 * 13 *", "the month field takes numbers from 1 to 12"),
            (
                "0 0 * * 7",
                "the day of the week field takes numbers from 0 to 6",
            ),
            ("0 0 * * MON", "the day of the week field takes numbers"),
            ("0 0 * JAN *", "the month field takes numbers"),
            ("0 0 L * *", "the day of the month field takes numbers"),
            ("0 0 ? * 1", "the day of the month field takes numbers"),
            ("5-1 * * * *", "runs backwards"),
            ("5/15 * * * *", "counts over `*` or a range"),
            ("*/0 * * * *", "is not a number from 1 to 59"),
            ("*/200 * * * *", "is not a number from 1 to 59"),
            ("0 9 1 * 1", "not both"),
            ("0 0 30 2 *", "names no day that exists"),
            ("0 0 31 4,6 *", "names no day that exists"),
        ] {
            let refusal = refusal(expression);
            assert!(
                refusal.contains(because),
                "{expression} was refused for the wrong reason: {refusal}"
            );
        }
    }

    #[test]
    fn a_time_zone_that_does_not_exist_is_refused() {
        let refusal = Cron::new("0 9 * * *", "Mars/Olympus_Mons")
            .expect_err("an unknown time zone should be refused");

        assert!(format!("{refusal:#}").contains("no time zone named \"Mars/Olympus_Mons\""));
    }
}
