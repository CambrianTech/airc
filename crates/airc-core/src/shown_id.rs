//! An id as a person or a model types it back: the full UUID, or the leading
//! hex every surface shows (`card 08ece9e8`, `claim=136f8174`).
//!
//! What a surface displays, its verbs must accept. The board shows short ids,
//! so a verb that demanded the full UUID bounced the id it had just printed.
//!
//! The id type carries the whole policy. [`Shown<T>`] is what a boundary
//! accepts for a `T` (`TryFrom<&str>`, and serde through it, so a bad id is
//! refused in the type's words before any verb runs). [`Shown::resolve`] turns
//! it into the `T` against the candidates the caller supplies (one room's
//! board, one card's claim): a prefix keeps every digit given, so an eight-digit
//! collision is settled by typing more, and a miss or a tie is refused with what
//! was found, never guessed. [`shown_form`] is the one producer of the short
//! rendering; each id's `shown()` calls it. Resolving an id grants nothing: the
//! verb still checks what it always did.
//!
//! Continuum's work verbs take the same types through the airc pin, so one id
//! means one thing on both surfaces.

use std::fmt;
use std::marker::PhantomData;

use serde::{Deserialize, Deserializer};
use uuid::Uuid;

/// The width every surface displays an id at: its leading hex characters.
pub const SHORT_ID_LEN: usize = 8;

/// Fewer hex digits than this leave nothing to tell candidates apart.
pub const MIN_PREFIX_HEX: usize = 4;

/// How many candidates a refusal names inline before it gives a count.
const MAX_LISTED_CANDIDATES: usize = 16;

/// The displayed short form of an id, exactly what a caller quotes back.
pub fn shown_form(id: Uuid) -> String {
    id.simple().to_string()[..SHORT_ID_LEN].to_string()
}

/// An id type a caller can name by its shown form.
pub trait ShownKind: Copy {
    /// What the id names, in refusals: `"card"`, `"claim"`.
    const LABEL: &'static str;
    fn from_uuid(id: Uuid) -> Self;
    fn to_uuid(self) -> Uuid;
}

/// A `T` as typed at a boundary, before it meets its candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown<T> {
    form: Form,
    kind: PhantomData<T>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// A full UUID (dashed or 32-hex): exact, used as given, candidates
    /// unread. A real head with an invented tail is NOT retried on its head:
    /// that would be repair, and the verb's own lookup refuses an unknown id.
    Full(Uuid),
    /// Leading hex digits, every one kept: the value of the first `len`
    /// nibbles, so a candidate matches when its top `len` nibbles equal it.
    Prefix { digits: u128, len: u8 },
}

impl Form {
    fn matches(self, id: Uuid) -> bool {
        match self {
            Self::Full(full) => full == id,
            Self::Prefix { digits, len } => id.as_u128() >> (4 * (32 - u32::from(len))) == digits,
        }
    }

    /// The prefix as the caller typed it (lowercased), for refusals.
    fn render(self) -> String {
        match self {
            Self::Full(id) => id.to_string(),
            Self::Prefix { digits, len } => format!("{digits:0width$x}", width = usize::from(len)),
        }
    }
}

/// Why a typed-back id did not name exactly one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShownIdError {
    /// Not a UUID and not a hex prefix of usable length.
    NotAnId { raw: String, label: &'static str },
    /// No candidate starts with the prefix; `shown` lists them (short form)
    /// when there are few enough to read, and `total` counts them all.
    NoMatch {
        prefix: String,
        label: &'static str,
        shown: Vec<String>,
        total: usize,
    },
    /// More than one candidate starts with the prefix.
    Ambiguous {
        prefix: String,
        label: &'static str,
        matches: Vec<String>,
    },
}

impl<T: ShownKind> TryFrom<&str> for Shown<T> {
    type Error = ShownIdError;

    /// No repair: a malformed id is refused, never trimmed into a prefix that
    /// might name something else.
    fn try_from(raw: &str) -> Result<Self, ShownIdError> {
        let raw = raw.trim();
        let prefix = || -> Option<Form> {
            if !(MIN_PREFIX_HEX..32).contains(&raw.len()) {
                return None;
            }
            // from_str_radix alone would take a leading '+'; every byte is hex.
            if !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            Some(Form::Prefix {
                digits: u128::from_str_radix(raw, 16).ok()?,
                len: u8::try_from(raw.len()).ok()?,
            })
        };
        let form = match Uuid::parse_str(raw) {
            Ok(id) => Form::Full(id),
            Err(_) => prefix().ok_or_else(|| ShownIdError::NotAnId {
                raw: raw.to_string(),
                label: T::LABEL,
            })?,
        };
        Ok(Self {
            form,
            kind: PhantomData,
        })
    }
}

impl<'de, T: ShownKind> Deserialize<'de> for Shown<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::try_from(raw.as_str()).map_err(serde::de::Error::custom)
    }
}

impl<T: ShownKind> Shown<T> {
    /// The id when it was given in full, so a caller can skip reading the
    /// candidates at all.
    pub fn full(&self) -> Option<T> {
        match &self.form {
            Form::Full(id) => Some(T::from_uuid(*id)),
            Form::Prefix { .. } => None,
        }
    }

    /// The one candidate this names. A full id passes through; a prefix must
    /// match exactly one candidate (the same id listed twice is one candidate).
    pub fn resolve(&self, candidates: impl IntoIterator<Item = T>) -> Result<T, ShownIdError> {
        if let Form::Full(id) = self.form {
            return Ok(T::from_uuid(id));
        }
        let prefix = self.form.render();
        let mut all: Vec<Uuid> = candidates.into_iter().map(T::to_uuid).collect();
        all.sort();
        all.dedup();
        let matches: Vec<Uuid> = all
            .iter()
            .copied()
            .filter(|id| self.form.matches(*id))
            .collect();
        match matches.as_slice() {
            [one] => Ok(T::from_uuid(*one)),
            [] => Err(ShownIdError::NoMatch {
                prefix,
                label: T::LABEL,
                shown: if all.len() <= MAX_LISTED_CANDIDATES {
                    all.iter().copied().map(shown_form).collect()
                } else {
                    Vec::new()
                },
                total: all.len(),
            }),
            many => Err(ShownIdError::Ambiguous {
                prefix,
                label: T::LABEL,
                matches: many.iter().map(|id| id.simple().to_string()).collect(),
            }),
        }
    }
}

impl fmt::Display for ShownIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnId { raw, label } => write!(
                f,
                "{raw:?} is not a usable {label} id: give the full id or at least \
                 {MIN_PREFIX_HEX} of its leading hex characters"
            ),
            Self::NoMatch {
                prefix,
                label,
                total: 0,
                ..
            } => write!(f, "no {label} matches {prefix:?}: there are none here"),
            Self::NoMatch {
                prefix,
                label,
                shown,
                total,
            } if shown.is_empty() => write!(
                f,
                "no {label} matches {prefix:?} among {total}: check the id you were shown"
            ),
            Self::NoMatch {
                prefix,
                label,
                shown,
                ..
            } => write!(
                f,
                "no {label} matches {prefix:?}; these are here: {}",
                shown.join(", ")
            ),
            Self::Ambiguous {
                prefix,
                label,
                matches,
            } => write!(
                f,
                "{label} {prefix:?} names {} ({}): give more characters",
                matches.len(),
                matches.join(", ")
            ),
        }
    }
}

impl std::error::Error for ShownIdError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Card(Uuid);

    impl ShownKind for Card {
        const LABEL: &'static str = "card";
        fn from_uuid(id: Uuid) -> Self {
            Self(id)
        }
        fn to_uuid(self) -> Uuid {
            self.0
        }
    }

    fn card(s: &str) -> Card {
        Card(Uuid::parse_str(s).unwrap())
    }

    // what this catches: the board prints short ids, so every work verb must
    // take one back; an eight-digit collision is settled by more digits (never
    // a guess); a malformed id is refused at the boundary (serde included), not
    // repaired into a prefix; a miss names what is here; a full id passes
    // without its candidates.
    #[test]
    fn a_shown_id_names_exactly_one_candidate_or_says_why_not() {
        let a = card("12345678-1000-4000-8000-000000000001");
        let b = card("12345678-2000-4000-8000-000000000002");
        let c = card("abcdef01-0000-4000-8000-000000000003");
        let resolve =
            |raw: &str, set: &[Card]| Shown::<Card>::try_from(raw)?.resolve(set.iter().copied());

        assert_eq!(resolve("abcdef01", &[a, b, c]), Ok(c));
        assert_eq!(resolve("ABCD", &[a, b, c]), Ok(c));
        assert_eq!(resolve("abcdef01", &[c, c]), Ok(c), "one id twice is one");
        assert!(matches!(
            resolve("12345678", &[a, b, c]),
            Err(ShownIdError::Ambiguous { .. })
        ));
        assert_eq!(resolve("123456781", &[a, b, c]), Ok(a));
        assert_eq!(
            resolve("1234567820", &[a, b, c]),
            Ok(b),
            "all ten digits count"
        );
        assert!(matches!(
            resolve("0000", &[a, b, c]),
            Err(ShownIdError::NoMatch { .. })
        ));
        // A real head with an invented tail is exact and unknown, not retried
        // on its head; the verb's own lookup refuses it.
        let invented = Uuid::parse_str("abcdef01-dead-4000-8000-00000000beef").unwrap();
        assert_eq!(resolve(&invented.to_string(), &[c]), Ok(Card(invented)));
        assert_eq!(resolve(&a.0.to_string(), &[]), Ok(a));
        assert_eq!(resolve(&a.0.simple().to_string(), &[]), Ok(a));

        // "placeholder" was filtered to its hex letters and accepted by the old
        // tolerant normalize(); here it is refused whole.
        for junk in [
            "placeholder",
            "12345678garbage",
            "123",
            "",
            "12345678-1000",
            "+abcd",
        ] {
            assert!(
                matches!(resolve(junk, &[a]), Err(ShownIdError::NotAnId { .. })),
                "{junk:?} must be refused, not repaired"
            );
        }
        let refused = serde_json::from_str::<Shown<Card>>("\"12345678garbage\"").unwrap_err();
        assert!(refused.to_string().contains("not a usable card id"));

        let miss = resolve("ffff", &[a, c]).unwrap_err().to_string();
        assert!(
            miss.contains("12345678") && miss.contains("abcdef01"),
            "{miss}"
        );
        assert_eq!(shown_form(c.0), "abcdef01");
    }
}
