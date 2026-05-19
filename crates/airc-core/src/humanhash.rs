//! Human-readable hash mnemonics used by invites and runtime agent labels.

use std::error::Error;
use std::fmt;

const WORDS: [&str; 256] = [
    "ack",
    "alabama",
    "alanine",
    "alaska",
    "alpha",
    "angel",
    "apart",
    "april",
    "arizona",
    "arkansas",
    "artist",
    "asparagus",
    "aspen",
    "august",
    "autumn",
    "avocado",
    "bacon",
    "bakerloo",
    "batman",
    "beer",
    "berlin",
    "beryllium",
    "black",
    "blossom",
    "blue",
    "bluebird",
    "bravo",
    "bulldog",
    "burger",
    "butter",
    "california",
    "carbon",
    "cardinal",
    "carolina",
    "carpet",
    "cat",
    "ceiling",
    "cello",
    "center",
    "charlie",
    "chicken",
    "coffee",
    "cola",
    "cold",
    "colorado",
    "comet",
    "connecticut",
    "crazy",
    "cup",
    "dakota",
    "december",
    "delaware",
    "delta",
    "diet",
    "don",
    "double",
    "early",
    "earth",
    "east",
    "echo",
    "edward",
    "eight",
    "eighteen",
    "eleven",
    "emma",
    "enemy",
    "equal",
    "failed",
    "fanta",
    "fillet",
    "finch",
    "fish",
    "five",
    "fix",
    "floor",
    "florida",
    "football",
    "four",
    "fourteen",
    "foxtrot",
    "freddie",
    "friend",
    "fruit",
    "gee",
    "georgia",
    "glucose",
    "golf",
    "green",
    "grey",
    "hamper",
    "happy",
    "harry",
    "hawaii",
    "helium",
    "high",
    "hot",
    "hotel",
    "hydrogen",
    "idaho",
    "illinois",
    "india",
    "indigo",
    "ink",
    "iowa",
    "island",
    "item",
    "jersey",
    "jig",
    "johnny",
    "juliet",
    "july",
    "jupiter",
    "kansas",
    "kentucky",
    "kilo",
    "king",
    "kitten",
    "lactose",
    "lake",
    "lamp",
    "lemon",
    "leopard",
    "lima",
    "lion",
    "lithium",
    "london",
    "louisiana",
    "low",
    "magazine",
    "magnesium",
    "maine",
    "mango",
    "march",
    "mars",
    "maryland",
    "massachusetts",
    "may",
    "mexico",
    "michigan",
    "mike",
    "minnesota",
    "mirror",
    "missouri",
    "mobile",
    "mockingbird",
    "monkey",
    "montana",
    "moon",
    "mountain",
    "muppet",
    "music",
    "nebraska",
    "neptune",
    "network",
    "nevada",
    "nine",
    "nineteen",
    "nitrogen",
    "north",
    "november",
    "nuts",
    "october",
    "ohio",
    "oklahoma",
    "one",
    "orange",
    "oranges",
    "oregon",
    "oscar",
    "oven",
    "oxygen",
    "papa",
    "paris",
    "pasta",
    "pennsylvania",
    "pip",
    "pizza",
    "pluto",
    "potato",
    "princess",
    "purple",
    "quebec",
    "queen",
    "quiet",
    "red",
    "river",
    "robert",
    "robin",
    "romeo",
    "rugby",
    "sad",
    "salami",
    "saturn",
    "september",
    "seven",
    "seventeen",
    "shade",
    "sierra",
    "single",
    "sink",
    "six",
    "sixteen",
    "skylark",
    "snake",
    "social",
    "sodium",
    "solar",
    "south",
    "spaghetti",
    "speaker",
    "spring",
    "stairway",
    "steak",
    "stream",
    "summer",
    "sweet",
    "table",
    "tango",
    "ten",
    "tennessee",
    "tennis",
    "texas",
    "thirteen",
    "three",
    "timing",
    "triple",
    "twelve",
    "twenty",
    "two",
    "uncle",
    "undress",
    "uniform",
    "uranus",
    "utah",
    "vegan",
    "venus",
    "vermont",
    "victor",
    "video",
    "violet",
    "virginia",
    "washington",
    "west",
    "whiskey",
    "white",
    "william",
    "winner",
    "winter",
    "wisconsin",
    "wolfram",
    "wyoming",
    "xray",
    "yankee",
    "yellow",
    "zebra",
    "zulu",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HumanhashError {
    EmptyInput,
    EmptyWordCount,
    InvalidHex,
}

impl fmt::Display for HumanhashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInput => f.write_str("empty input"),
            Self::EmptyWordCount => f.write_str("word count must be >= 1"),
            Self::InvalidHex => f.write_str("input must be hex"),
        }
    }
}

impl Error for HumanhashError {}

pub fn humanhash(hex_input: &str, word_count: usize) -> Result<String, HumanhashError> {
    if hex_input.is_empty() {
        return Err(HumanhashError::EmptyInput);
    }
    if word_count == 0 {
        return Err(HumanhashError::EmptyWordCount);
    }

    let mut normalized = String::with_capacity(hex_input.len() + (hex_input.len() % 2));
    if hex_input.len() % 2 == 1 {
        normalized.push('0');
    }
    normalized.push_str(hex_input);

    let data = decode_hex(&normalized)?;
    if data.is_empty() {
        return Err(HumanhashError::EmptyInput);
    }

    let segment_size = usize::max(data.len() / word_count, 1);
    let mut output = Vec::with_capacity(word_count);
    for segment in 0..word_count {
        let start = segment * segment_size;
        let end = if segment == word_count - 1 {
            data.len()
        } else {
            start + segment_size
        };
        let acc = data
            .get(start..end)
            .unwrap_or(&[])
            .iter()
            .fold(0u8, |acc, value| acc ^ value);
        output.push(WORDS[acc as usize]);
    }

    Ok(output.join("-"))
}

fn decode_hex(input: &str) -> Result<Vec<u8>, HumanhashError> {
    input
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_value(pair[0])?;
            let low = hex_value(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_value(value: u8) -> Result<u8, HumanhashError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(HumanhashError::InvalidHex),
    }
}

#[cfg(test)]
mod tests {
    use super::{humanhash, HumanhashError};

    #[test]
    fn known_mnemonic_matches_legacy_python() {
        assert_eq!(
            humanhash("d7e247c0000000000000000000000000", 4).unwrap(),
            "potato-ack-ack-ack"
        );
    }

    #[test]
    fn odd_hex_is_left_padded() {
        assert_eq!(humanhash("abc", 2), humanhash("0abc", 2));
    }

    #[test]
    fn invalid_inputs_fail_explicitly() {
        assert_eq!(humanhash("", 4), Err(HumanhashError::EmptyInput));
        assert_eq!(humanhash("abcd", 0), Err(HumanhashError::EmptyWordCount));
        assert_eq!(humanhash("not-hex", 4), Err(HumanhashError::InvalidHex));
    }
}
