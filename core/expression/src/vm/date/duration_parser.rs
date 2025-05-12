use crate::vm::date::duration::Duration;
use crate::vm::date::duration_unit::DurationUnit;
use rust_decimal::prelude::ToPrimitive;
use std::str::Chars;
use thiserror::Error;

#[derive(Debug, PartialEq, Clone, Error)]
pub(crate) enum DurationParseError {
    #[error("Invalid character at {0}")]
    InvalidCharacter(usize),
    #[error("Expected number at {0}")]
    NumberExpected(usize),
    #[error("Unknown time unit {unit}")]
    UnknownUnit {
        start: usize,
        end: usize,
        value: u64,
        unit: String,
    },
    #[error("Number is too large")]
    NumberOverflow,
    #[error("Value is empty")]
    Empty,
}

pub(crate) struct DurationParser<'a> {
    pub iter: Chars<'a>,
    pub src: &'a str,
    pub duration: Duration,
}

impl DurationParser<'_> {
    fn off(&self) -> usize {
        self.src.len() - self.iter.as_str().len()
    }

    fn parse_first_char(&mut self) -> Result<Option<u64>, DurationParseError> {
        let off = self.off();
        for c in self.iter.by_ref() {
            match c {
                '0'..='9' => {
                    return Ok(Some(c as u64 - '0' as u64));
                }
                c if c.is_whitespace() => continue,
                _ => {
                    return Err(DurationParseError::NumberExpected(off));
                }
            }
        }
        Ok(None)
    }
    fn parse_unit(&mut self, n: u64, start: usize, end: usize) -> Result<(), DurationParseError> {
        let unit = DurationUnit::parse(&self.src[start..end]).ok_or_else(|| {
            DurationParseError::UnknownUnit {
                start,
                end,
                unit: self.src[start..end].to_string(),
                value: n,
            }
        })?;

        match unit {
            DurationUnit::Quarter => {
                self.duration.months = n.to_i32().ok_or(DurationParseError::NumberOverflow)? * 3
            }
            DurationUnit::Month => {
                self.duration.months = n.to_i32().ok_or(DurationParseError::NumberOverflow)?
            }
            DurationUnit::Year => {
                self.duration.years = n.to_i32().ok_or(DurationParseError::NumberOverflow)?
            }
            _ => {
                // No-op
            }
        }

        match unit.as_secs() {
            Some(secs) => {
                self.duration.seconds = self
                    .duration
                    .seconds
                    .checked_add(
                        secs.to_i64()
                            .ok_or(DurationParseError::NumberOverflow)?
                            .checked_mul(n.to_i64().ok_or(DurationParseError::NumberOverflow)?)
                            .ok_or(DurationParseError::NumberOverflow)?,
                    )
                    .ok_or(DurationParseError::NumberOverflow)?
            }
            None => {
                // No-op
            }
        };

        Ok(())
    }

    pub fn parse(mut self) -> Result<Duration, DurationParseError> {
        let mut n = self.parse_first_char()?.ok_or(DurationParseError::Empty)?;
        'outer: loop {
            let mut off = self.off();
            while let Some(c) = self.iter.next() {
                match c {
                    '0'..='9' => {
                        n = n
                            .checked_mul(10)
                            .and_then(|x| x.checked_add(c as u64 - '0' as u64))
                            .ok_or(DurationParseError::NumberOverflow)?;
                    }
                    c if c.is_whitespace() => {}
                    'a'..='z' | 'A'..='Z' => {
                        break;
                    }
                    _ => {
                        return Err(DurationParseError::InvalidCharacter(off));
                    }
                }
                off = self.off();
            }
            let start = off;
            let mut off = self.off();
            while let Some(c) = self.iter.next() {
                match c {
                    '0'..='9' => {
                        self.parse_unit(n, start, off)?;
                        n = c as u64 - '0' as u64;
                        continue 'outer;
                    }
                    c if c.is_whitespace() => break,
                    'a'..='z' | 'A'..='Z' => {}
                    _ => {
                        return Err(DurationParseError::InvalidCharacter(off));
                    }
                }
                off = self.off();
            }
            self.parse_unit(n, start, off)?;
            n = match self.parse_first_char()? {
                Some(n) => n,
                None => return Ok(self.duration),
            };
        }
    }
}
