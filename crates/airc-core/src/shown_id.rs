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

/// Where a kind of id lives: the `T`s a caller can name in one scope (one room's
/// board, one card's submissions, the live personas). Each concern implements it
/// once, beside the data it already owns; a verb then resolves with
/// [`Shown::resolve_in`] and never builds a candidate list of its own.
pub trait ShownScope<T: ShownKind> {
    /// Every `T` named here. The same id listed twice is one candidate.
    fn shown_ids(&self) -> Vec<T>;
}

/// A plain list of ids is a scope, for a caller whose population is already in hand.
impl<T: ShownKind> ShownScope<T> for [T] {
    fn shown_ids(&self) -> Vec<T> {
        self.to_vec()
    }
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

    /// No repair of a well-formed id: an exact UUID is used as given, never
    /// retried on its head. A UUID one hex digit off (a model added or dropped
    /// one) is not a UUID at all, so it is read as the short form it leads with
    /// ([`mistyped_uuid`]); like any prefix it names only one of the caller's own
    /// candidates or is refused.
    fn try_from(raw: &str) -> Result<Self, ShownIdError> {
        let raw = raw.trim();
        let prefix = |hex: &str| -> Option<Form> {
            if !(MIN_PREFIX_HEX..32).contains(&hex.len()) {
                return None;
            }
            // from_str_radix alone would take a leading '+'; every byte is hex.
            if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            Some(Form::Prefix {
                digits: u128::from_str_radix(hex, 16).ok()?,
                len: u8::try_from(hex.len()).ok()?,
            })
        };
        let form =
            match Uuid::parse_str(raw) {
                Ok(id) => Form::Full(id),
                Err(_) => prefix(raw).or_else(|| mistyped_uuid(raw)).ok_or_else(|| {
                    ShownIdError::NotAnId {
                        raw: raw.to_string(),
                        label: T::LABEL,
                    }
                })?,
            };
        Ok(Self {
            form,
            kind: PhantomData,
        })
    }
}

/// What a UUID written with odd separators or one hex digit off reads as. `raw`
/// must be only hex digits and dashes, so words around an id are never mined for
/// hex (`card d7cf...` is not read as `cad...`, which continuum's `normalize` did).
/// - 32 hex digits under misplaced or doubled dashes are every digit of one id:
///   that exact id, never a repair (the verb's own lookup refuses an unknown id).
/// - 31 or 33 hex digits, one dropped or added by whoever copied it, are read by
///   the 8-hex short form they lead with. On 2026-07-13 that was 28% of live
///   `work/claim` calls (`d7cfe47e0-8e39-...`, a nine-digit first group).
///   Continuum's persona-id verbs read it this way through `id_resolve::resolve`;
///   this is that reading in the one id type, so they can move here without
///   losing it. Like any prefix it names only one of the caller's candidates.
/// - Anything else (30 or 34 digits, a fragment) is not an id here.
fn mistyped_uuid(raw: &str) -> Option<Form> {
    if !raw.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return None;
    }
    let hex: String = raw.chars().filter(char::is_ascii_hexdigit).collect();
    match hex.len() {
        32 => Uuid::parse_str(&hex).ok().map(Form::Full),
        31 | 33 => {
            let head = raw.get(..SHORT_ID_LEN)?;
            if !head.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            Some(Form::Prefix {
                digits: u128::from_str_radix(head, 16).ok()?,
                len: u8::try_from(SHORT_ID_LEN).ok()?,
            })
        }
        _ => None,
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

    /// The one `T` this names in `scope`. A full id passes without reading it.
    pub fn resolve_in<S: ShownScope<T> + ?Sized>(&self, scope: &S) -> Result<T, ShownIdError> {
        match self.full() {
            Some(id) => Ok(id),
            None => self.resolve(scope.shown_ids()),
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

/// The pattern of every id [`Shown::try_from`] reads exactly: a full UUID (32 hex, bare or dashed)
/// or a hex prefix of at least [`MIN_PREFIX_HEX`] digits. Built from the parser's own bounds, so the
/// schema a tool offers and the decoder that reads the call cannot drift. It is a SUBSET of what the
/// parser takes (a mistyped 31- or 33-digit UUID still reads as its short form, [`mistyped_uuid`]),
/// so every value a model generates from it decodes.
pub fn shown_pattern() -> String {
    let hex = "[0-9a-fA-F]";
    format!("^(?:{hex}{{{MIN_PREFIX_HEX},32}}|{hex}{{8}}-{hex}{{4}}-{hex}{{4}}-{hex}{{4}}-{hex}{{12}})$")
}

/// A Shown id is offered as the string its parser reads, never an arbitrary one (Codex's all-tool
/// schema gate, 9ad69: a sample from `Option<String>` was refused by `Shown<T>`).
#[cfg(feature = "schemars")]
impl<T: ShownKind> schemars::JsonSchema for Shown<T> {
    fn schema_name() -> String {
        format!("Shown_{}", T::LABEL)
    }

    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        use schemars::schema::{InstanceType, Metadata, SchemaObject, StringValidation};
        SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            string: Some(Box::new(StringValidation {
                pattern: Some(shown_pattern()),
                ..Default::default()
            })),
            metadata: Some(Box::new(Metadata {
                description: Some(format!(
                    "a {} id: its full UUID or at least {MIN_PREFIX_HEX} of its leading hex digits",
                    T::LABEL
                )),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

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

    // what this catches: the offered pattern and the parser drifting apart. Every id the pattern
    // admits (each boundary length, bare and dashed full ids, either case) must decode, and the
    // forms just outside it (3 digits, 33 without dashes, a word) must not match it.
    #[test]
    fn every_id_the_shown_pattern_admits_decodes() {
        let Ok(pattern) = regex::Regex::new(&shown_pattern()) else {
            panic!("the pattern compiles")
        };
        let full = "08ece9e8-1d2c-4b3a-9f8e-7d6c5b4a3f2e";
        let admitted = [
            "08ec".to_string(),
            "08ECE9E8".to_string(),
            "0".repeat(31),
            full.replace('-', ""),
            full.to_string(),
            full.to_uppercase(),
        ];
        for id in &admitted {
            assert!(pattern.is_match(id), "{id} is a Shown id the parser reads");
            assert!(
                Shown::<Card>::try_from(id.as_str()).is_ok(),
                "{id} matches the pattern and must decode"
            );
        }
        for outside in ["08e", &"a".repeat(33), "card-08ece9e8", "08ece9e8 "] {
            assert!(
                !pattern.is_match(outside),
                "{outside:?} is outside the offered pattern"
            );
        }
    }

    // what this catches: the schema a tool offers for a Shown id losing the parser's pattern (an
    // arbitrary string again) or being shared by reference across kinds that name different ids.
    #[cfg(feature = "schemars")]
    #[test]
    fn a_shown_id_is_offered_as_its_pattern() {
        let schema = schemars::schema_for!(Shown<Card>);
        let Ok(value) = serde_json::to_value(&schema) else {
            panic!("a schema serializes")
        };
        assert_eq!(value["type"], "string");
        assert_eq!(value["pattern"].as_str(), Some(shown_pattern().as_str()));
        assert_eq!(value["title"], "Shown_card");
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

    // what this catches (2026-07-13: 28% of live work/claim calls): a model that copies a full
    // UUID one hex digit off (an added or dropped digit) is refused outright, where continuum's
    // id_resolve::resolve (its persona-id verbs) read it by its leading short form. Read here
    // the same way, it still names only one of the caller's own candidates: a tie is refused, a
    // dropped digit inside the leading group (no clean short form) is refused, and a well-formed
    // id is never repaired.
    #[test]
    fn a_uuid_one_digit_off_is_read_by_its_short_form_against_the_candidates() {
        let resolve =
            |raw: &str, set: &[Card]| Shown::<Card>::try_from(raw)?.resolve(set.iter().copied());
        let target = card("d7cfe47e-8e39-4a1b-9c2d-0123456789ab");
        let other = card("12345678-0000-4000-8000-000000000000");
        // An added digit (a nine-digit first group) and a dropped one in a later group.
        assert_eq!(
            resolve("d7cfe47e0-8e39-4a1b-9c2d-0123456789ab", &[target, other]),
            Ok(target)
        );
        assert_eq!(
            resolve("d7cfe47e-8e39-4a1b-9c2d-012345678ab", &[target, other]),
            Ok(target)
        );
        // A 33-digit run with no dashes is the same mistake.
        assert_eq!(
            resolve("d7cfe47e08e394a1b9c2d0123456789ab", &[target]),
            Ok(target)
        );
        // Still only a prefix: a tie on the short form is refused, never guessed.
        let twin = card("d7cfe47e-ffff-4fff-8fff-ffffffffffff");
        assert!(matches!(
            resolve("d7cfe47e0-8e39-4a1b-9c2d-0123456789ab", &[target, twin]),
            Err(ShownIdError::Ambiguous { .. })
        ));
        // A digit dropped inside the leading group leaves no clean short form: refused.
        assert!(matches!(
            resolve("d7cfe47-8e39-4a1b-9c2d-0123456789ab", &[target]),
            Err(ShownIdError::NotAnId { .. })
        ));
        // Every digit of one id under doubled or misplaced dashes is that exact id.
        assert_eq!(
            resolve("d7cfe47e--8e39-4a1b-9c2d-0123456789ab", &[]),
            Ok(target)
        );
        assert_eq!(
            resolve("d7cf-e47e8e39-4a1b9c2d-0123456789ab", &[]),
            Ok(target)
        );
        // Words or spaces around an id are not mined for its hex.
        assert!(matches!(
            resolve("card d7cfe47e0-8e39-4a1b-9c2d-0123456789ab", &[target]),
            Err(ShownIdError::NotAnId { .. })
        ));
        // A well-formed id is used exactly as given, never retried on its head.
        let invented = "d7cfe47e-8e39-4a1b-9c2d-ffffffffffff";
        assert_ne!(
            resolve(invented, &[target]),
            Ok(target),
            "a well-formed id is never repaired"
        );
    }

    // what this catches (Joel 2026-11-04: one lookup per concern, automatic): a scope
    // resolves a shown id exactly as a candidate list does, and a full id passes without
    // the scope being read at all, so a verb on an unreadable board still takes a full id.
    #[test]
    fn a_scope_resolves_a_shown_id_and_a_full_id_never_reads_it() {
        struct Counted(std::cell::Cell<u32>, Vec<Card>);
        impl ShownScope<Card> for Counted {
            fn shown_ids(&self) -> Vec<Card> {
                self.0.set(self.0.get() + 1);
                self.1.clone()
            }
        }
        let a = card("12345678-1000-4000-8000-000000000001");
        let c = card("abcdef01-0000-4000-8000-000000000003");
        let scope = Counted(std::cell::Cell::new(0), vec![a, c]);
        let shown = |raw: &str| Shown::<Card>::try_from(raw).unwrap();

        assert_eq!(shown("abcdef01").resolve_in(&scope), Ok(c));
        assert_eq!(scope.0.get(), 1, "a prefix reads its scope");
        assert_eq!(shown(&a.0.to_string()).resolve_in(&scope), Ok(a));
        assert_eq!(scope.0.get(), 1, "a full id never reads it");
        assert_eq!(
            shown("abcdef01").resolve_in(&[a, c][..]),
            Ok(c),
            "a slice is a scope"
        );
        assert!(matches!(
            shown("0000").resolve_in(&scope),
            Err(ShownIdError::NoMatch { total: 2, .. })
        ));
    }
}
