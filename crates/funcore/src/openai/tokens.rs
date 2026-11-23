// The language model's token counts and their price.

use super::models::Model;
use crate::base::json::{number_at, text_at};
use alloc::format;
use alloc::string::String;

const LONG_CONTEXT_TOKENS: u64 = 272_000;
const RATE_UNIT: u64 = 1000;

// Millionths of a dollar a thousand tokens, from OpenAI's price pages.
const fn rates(model: Model, is_long: bool) -> Rates {
    let [plain, cache_read, cache_write, cache_write_12h, output] = match (model, is_long) {
        (Model::Luna, false) => [100, 10, 125, 250, 500],
        (Model::Luna, true) => [200, 20, 250, 500, 750],
        (Model::Terra, false) => [1_000, 50, 1_250, 2_500, 5_000],
        (Model::Terra, true) => [2_000, 100, 2_500, 5_000, 7_500],
        (Model::Sol, false) => [2_000, 100, 2_500, 5_000, 10_000],
        (Model::Sol, true) => [4_000, 200, 5_000, 10_000, 15_000],
        (Model::Astra, false) => [10_000, 1_000, 12_500, 25_000, 50_000],
        (Model::Astra, true) => [20_000, 2_000, 25_000, 50_000, 75_000],
    };
    Rates { plain, cache_read, cache_write, cache_write_12h, output }
}

struct Rates {
    plain: u64,
    cache_read: u64,
    cache_write: u64,
    cache_write_12h: u64,
    output: u64,
}

/// The tokens of an answer: all input, those read from the cache, those written into it, of which those stored 12 hours, and the output, thought included; and the model that answered, which prices them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Usage {
    pub input: u64,
    pub cache_reads: u64,
    pub cache_writes: u64,
    pub cache_writes_12h: u64,
    pub(super) output: u64,
    pub model: Model,
}

impl Usage {
    pub(super) fn parse(response: &str) -> Usage {
        let count = |path: &[&str]| number_at::<u64>(response, path).unwrap_or(0);
        Usage {
            input: count(&["usage", "input_tokens"]),
            cache_reads: count(&["usage", "input_tokens_details", "cached_tokens"]),
            cache_writes: count(&["usage", "input_tokens_details", "cache_write_tokens"]),
            cache_writes_12h: count(&["usage", "input_tokens_details", "cache_write_tokens_details", "ttl_12h_tokens"]),
            output: count(&["usage", "output_tokens"]),
            model: text_at(response, &["model"]).map_or_else(Model::default, Model::named),
        }
    }

    pub(super) fn is_counted(&self) -> bool {
        Usage { model: Model::default(), ..*self } != Usage::default()
    }

    // The cost in millionths of a dollar, to the nearest; a count no answer could hold costs the most there is.
    pub(super) const fn micros(&self) -> u64 {
        let rates = rates(self.model, self.input > LONG_CONTEXT_TOKENS);
        let plain = self.input.saturating_sub(self.cache_reads).saturating_sub(self.cache_writes);
        let long_writes =
            if self.cache_writes_12h < self.cache_writes { self.cache_writes_12h } else { self.cache_writes };
        let units = plain
            .saturating_mul(rates.plain)
            .saturating_add(self.cache_reads.saturating_mul(rates.cache_read))
            .saturating_add((self.cache_writes - long_writes).saturating_mul(rates.cache_write))
            .saturating_add(long_writes.saturating_mul(rates.cache_write_12h))
            .saturating_add(self.output.saturating_mul(rates.output));
        units.saturating_add(RATE_UNIT / 2) / RATE_UNIT
    }

    /// The log text of the usage and its cost.
    pub fn to_log_line(self) -> String {
        format!(
            "{} tokens in, {} of them read from the cache and {} written into it, {} out, {} millionths of a dollar by {}",
            self.input,
            self.cache_reads,
            self.cache_writes,
            self.output,
            self.micros(),
            self.model.to_word()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_and_cost() {
        let done = r#"{"status": "completed", "usage": {"input_tokens": 51234, "input_tokens_details": {"cached_tokens": 50000, "cache_write_tokens": 1000}, "output_tokens": 2100, "output_tokens_details": {"reasoning_tokens": 1800}, "total_tokens": 53334}}"#;
        let usage = Usage::parse(done);
        assert_eq!(
            usage,
            Usage {
                input: 51_234,
                cache_reads: 50_000,
                cache_writes: 1_000,
                cache_writes_12h: 0,
                output: 2_100,
                model: Model::Luna
            }
        );
        assert_eq!(
            usage.micros(),
            1_698,
            "234 in at $0.10 a million, 50,000 read from the cache at $0.01, 1,000 written into it at $0.125, 2,100 out at $0.50"
        );
        assert_eq!(
            (
                Usage { input: 1450, output: 2439, ..Usage::default() }.micros(),
                Usage { input: 1450, cache_writes: 1450, output: 2439, ..Usage::default() }.micros()
            ),
            (1365, 1401),
            "a summary whose words the cache did not have, and the same written into it"
        );
        assert_eq!(
            [0, 4, 5].map(|input| Usage { input, ..Usage::default() }.micros()),
            [0, 0, 1],
            "to the nearest millionth"
        );
        assert_eq!(
            Usage { input: 10, cache_reads: 8, cache_writes: 8, ..Usage::default() }.micros(),
            1,
            "a count of more read and written than went in leaves nothing at the plain rate"
        );
        let long_writes = r#"{"usage": {"input_tokens": 64000, "input_tokens_details": {"cached_tokens": 60000, "cache_write_tokens": 4000, "cache_write_tokens_details": {"ttl_12h_tokens": 3000, "ttl_30m_tokens": 1000}}, "output_tokens": 0}}"#;
        assert_eq!(
            Usage::parse(long_writes),
            Usage {
                input: 64_000,
                cache_reads: 60_000,
                cache_writes: 4_000,
                cache_writes_12h: 3_000,
                output: 0,
                model: Model::Luna
            }
        );
        assert_eq!(
            Usage::parse(long_writes).micros(),
            1_475,
            "60,000 read from the cache at $0.01 a million, 1,000 written into it for 30 minutes at $0.125, 3,000 for 12 hours at $0.25"
        );
        assert_eq!(
            Usage { input: 10, cache_writes: 4, cache_writes_12h: 9, ..Usage::default() }.micros(),
            2,
            "of more kept 12 hours than were written, those written"
        );
        assert_eq!(
            Usage { input: u64::MAX, output: u64::MAX, ..Usage::default() }.micros(),
            u64::MAX / RATE_UNIT,
            "a count no answer could hold costs the most there is, never what is left of it once it wraps"
        );
        for body in [r#"{"status": "in_progress", "usage": null}"#, "{}", "not json"] {
            assert_eq!(Usage::parse(body), Usage::default(), "{body}");
        }
        assert!(
            usage.is_counted()
                && Usage { output: 1, ..Usage::default() }.is_counted()
                && !Usage::default().is_counted()
                && !Usage { model: Model::Astra, ..Usage::default() }.is_counted()
        );
        assert_eq!(
            usage.to_log_line(),
            "51234 tokens in, 50000 of them read from the cache and 1000 written into it, 2100 out, 1698 millionths of a dollar by gpt-6-luna"
        );
        assert_eq!(
            Usage::parse(
                r#"{"usage": {"input_tokens": 1450, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 2439}}"#
            ),
            Usage { input: 1450, output: 2439, ..Usage::default() },
            "an answer that tells no writing into the cache had none"
        );
    }

    #[test]
    fn rates_of_answering_model() {
        let answered = |model: &str| {
            Usage::parse(&format!(
                r#"{{"model": "{model}", "usage": {{"input_tokens": 100000, "input_tokens_details": {{"cached_tokens": 50000}}, "output_tokens": 10000}}}}"#
            ))
        };
        assert_eq!(
            ["gpt-6-luna-2026-10-01", "gpt-6.1-terra", "gpt-6.1-sol", "gpt-6-astra", "gpt-7"]
                .map(|model| answered(model).micros()),
            [10_500, 102_500, 205_000, 1_050_000, 10_500],
            "50,000 in, 50,000 read from the cache and 10,000 out, at the rates of the model that answered, and a model of no known name at the cheapest"
        );
        let long = Usage { input: 300_000, output: 1_000, model: Model::Sol, ..Usage::default() };
        assert_eq!(long.micros(), 1_215_000, "past 272,000 tokens in, the model's long rates");
    }

    #[test]
    fn long_context_rates() {
        let micros = |input, cache_reads, cache_writes, output| {
            Usage { input, cache_reads, cache_writes, output, ..Usage::default() }.micros()
        };
        assert_eq!(micros(100_000, 0, 0, 10_000), 15_000, "a rundown of a cent and a half");
        assert_eq!(micros(272_000, 0, 0, 10_000), 32_200, "at the border still the usual rates");
        assert_eq!(
            micros(272_001, 0, 0, 10_000),
            61_900,
            "past it twice for what goes in and half again for what comes out"
        );
        assert_eq!(micros(400_000, 0, 0, 20_000), 95_000);
        assert_eq!(micros(300_000, 0, 300_000, 1_000), 75_750, "written into the cache at $0.25 a million");
        assert_eq!(micros(300_000, 290_000, 10_000, 1_000), 9_050, "read from it at $0.02");
        assert_eq!(
            Usage {
                input: 300_000,
                cache_writes: 300_000,
                cache_writes_12h: 300_000,
                output: 1_000,
                ..Usage::default()
            }
            .micros(),
            150_750,
            "and written into it for 12 hours at $0.50"
        );
    }
}
