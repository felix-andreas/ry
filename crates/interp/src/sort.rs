//! Sorting, ordering, ranking, matching, duplicate detection, `split` and `identical`.
//!
//! Strings order by code point, which is GNU R's order under `LC_COLLATE=C`. Every ordering
//! here that GNU R documents as stable is stable, and the internals that GNU R implements with
//! an unstable algorithm (`sort` on doubles, `psort`, `qsort`) use the same textbook algorithm,
//! because the arrangement of ties and of `NaN` is observable through them.

use {
    crate::{
        builtins::{BuiltinDef, Call, internal},
        coerce::{
            CoercionWarnings, coerce_vector, factor_as_character, integer_from_string, real_to_int,
        },
        conditions::warning,
        eval, records, standard_library,
        subset::select_elements,
        unwind::{R, error},
        values::{
            self, alloc, as_logical_scalar, char_str, get_attr, length, na_string, nil, string_at,
            symbols,
        },
    },
    heap::{
        object::{
            NA_INTEGER, NA_LOGICAL, NA_REAL_BITS, Rcomplex, Sexp, SexpType, Slot, gp, is_na_real,
        },
        records::NativeKind,
    },
    icu_collator::{
        CollatorBorrowed, CollatorPreferences,
        options::{AlternateHandling, CaseLevel, CollatorOptions, Strength},
        preferences::CollationCaseFirst,
    },
    icu_locale_core::Locale,
    rustc_hash::{FxHashMap, FxHashSet, FxHasher},
    smallvec::{SmallVec, smallvec},
    std::{
        borrow::Cow,
        cell::Cell,
        cmp::Ordering,
        hash::{Hash, Hasher},
        sync::{
            PoisonError, RwLock, RwLockReadGuard,
            atomic::{AtomicU8, Ordering as AtomicOrdering},
        },
    },
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("merge", 4, do_merge),
    internal("sort", 2, do_sort),
    internal("psort", 2, do_psort),
    internal("qsort", 2, do_qsort),
    internal("order", -1, do_order),
    internal("radixsort", -1, do_radixsort),
    internal("rank", 3, do_rank),
    internal("unique", 4, do_unique),
    internal("duplicated", 4, do_duplicated),
    internal("anyDuplicated", 3, do_any_duplicated),
    internal("match", 4, do_match),
    internal("rowsum_matrix", 5, do_rowsum_matrix),
    internal("rowsum_df", 5, do_rowsum_data_frame),
    internal("split", 2, do_split),
    internal("identical", 9, do_identical),
    internal("sorted_fpass", 3, do_sorted_fpass),
    internal("icuSetCollate", -1, do_icu_set_collate).invisible(),
    internal("icuGetCollate", 1, do_icu_get_collate),
    // The libR host replaces this with a real wrapper for package code.
    internal("wrap_meta", 3, |call| Ok(call.arg(0))),
];

/// Compares two strings as GNU R's `Scollate` does. In the `C` locale that is code point order.
/// Otherwise GNU R collates with an ICU collator for the collation locale, which is the CLDR
/// root collation for `C.UTF-8` and `en_US.UTF-8`; strings that collator calls equal (`"é"` and
/// `"e\u{301}"`, or a string and itself with a zero-width space) compare equal.
pub fn collate(left: &str, right: &str) -> Ordering {
    match MODE.load(AtomicOrdering::Relaxed) {
        CODE_POINT => left.cmp(right),
        ROOT_ASCII if is_printable_ascii(left) && is_printable_ascii(right) => {
            ascii_collate(left.as_bytes(), right.as_bytes())
        }
        _ => collator().compare(left, right),
    }
}

/// The session's collation, held for a run of comparisons so a sort reads it once.
pub struct Collator(RwLockReadGuard<'static, Collation>);

pub fn collator() -> Collator {
    Collator(COLLATION.read().unwrap_or_else(PoisonError::into_inner))
}

impl Collator {
    pub fn compare(&self, left: &str, right: &str) -> Ordering {
        match &*self.0 {
            Collation::CodePoint { .. } => left.cmp(right),
            Collation::Strcoll => strcoll(left, right),
            Collation::Icu(icu) => {
                if icu.ascii_order && is_printable_ascii(left) && is_printable_ascii(right) {
                    return ascii_collate(left.as_bytes(), right.as_bytes());
                }
                // ICU reads an invalid UTF-8 sequence as U+FFFD, as `compare_utf8` does.
                match (
                    values::unescape_invalid_bytes(left),
                    values::unescape_invalid_bytes(right),
                ) {
                    (None, None) => icu.collator.compare(left, right),
                    (left_bytes, right_bytes) => icu.collator.compare_utf8(
                        left_bytes.as_deref().unwrap_or(left.as_bytes()),
                        right_bytes.as_deref().unwrap_or(right.as_bytes()),
                    ),
                }
            }
        }
    }

    /// One ICU sort key per text, when ICU collates: their byte order is [`Self::compare`]'s,
    /// and equal keys mean equal texts at the collator's strength. A key holds no zero byte.
    fn keys<Text: AsRef<str>>(&self, texts: &[Text]) -> Option<CollationKeys> {
        let Collation::Icu(icu) = &*self.0 else {
            return None;
        };
        let mut bytes = Vec::new();
        let mut starts = Vec::with_capacity(texts.len() + 1);
        for text in texts {
            starts.push(u32::try_from(bytes.len()).ok()?);
            let text = text.as_ref();
            let Ok(()) = match values::unescape_invalid_bytes(text) {
                None => icu.collator.write_sort_key_to(text, &mut bytes),
                Some(raw) => icu.collator.write_sort_key_utf8_to(&raw, &mut bytes),
            };
        }
        starts.push(u32::try_from(bytes.len()).ok()?);
        Some(CollationKeys { bytes, starts })
    }
}

pub fn sort_strings(names: &mut [String]) {
    let order = collation_order(names);
    let sorted: Vec<String> = order
        .iter()
        .map(|&index| std::mem::take(&mut names[index as usize]))
        .collect();
    for (slot, name) in names.iter_mut().zip(sorted) {
        *slot = name;
    }
}

/// The permutation that sorts `texts` as [`collate`] orders them, keeping equal texts in their
/// original order.
pub fn collation_order<Text: AsRef<str>>(texts: &[Text]) -> Vec<u32> {
    let mut order: Vec<u32> = (0..texts.len() as u32).collect();
    let text = |index: u32| texts[index as usize].as_ref();
    if c_collation() {
        order.sort_by(|&left, &right| text(left).cmp(text(right)));
        return order;
    }
    let collator = collator();
    match CollationKeys::ascii(texts).or_else(|| collator.keys(texts)) {
        Some(keys) => order.sort_by(|&left, &right| keys.key(left).cmp(keys.key(right))),
        None => order.sort_by(|&left, &right| collator.compare(text(left), text(right))),
    }
    order
}

/// Whether strings collate by code point, so a byte comparison sorts them.
fn c_collation() -> bool {
    MODE.load(AtomicOrdering::Relaxed) == CODE_POINT
}

/// How strings compare, as GNU R's `collator` and `collationLocaleSet` in `util.c` decide.
pub enum Collation {
    /// Byte order: the `C` locale, or `icuSetCollate(locale = "ASCII")` (`ascii`).
    CodePoint {
        ascii: bool,
    },
    /// The C library's `strcoll`, where GNU R has no ICU collator but the locale is not `C`.
    Strcoll,
    Icu(IcuCollation),
}

pub struct IcuCollation {
    collator: CollatorBorrowed<'static>,
    /// The locale `icuGetCollate()` reports: `root`, or the name the collator was opened for.
    locale: String,
    preferences: CollatorPreferences,
    options: CollatorOptions,
    /// Whether the locale orders printable ASCII as the root collation does.
    root_ascii_locale: bool,
    /// Whether two strings of printable ASCII order as `ascii_collate` orders them: the locale
    /// does, and no attribute changes it.
    ascii_order: bool,
}

/// The one collation object, rebuilt only when the collation locale or `icuSetCollate` changes
/// it. `MODE` mirrors its kind so a comparison of two ASCII strings needs no lock.
static COLLATION: RwLock<Collation> = RwLock::new(Collation::CodePoint { ascii: false });
static MODE: AtomicU8 = AtomicU8::new(CODE_POINT);
const CODE_POINT: u8 = 0;
const ROOT_ASCII: u8 = 1;
const GENERAL: u8 = 2;

fn install(collation: Collation) {
    let mode = match &collation {
        Collation::CodePoint { .. } => CODE_POINT,
        Collation::Icu(icu) if icu.ascii_order => ROOT_ASCII,
        _ => GENERAL,
    };
    let mut current = COLLATION.write().unwrap_or_else(PoisonError::into_inner);
    *current = collation;
    MODE.store(mode, AtomicOrdering::Relaxed);
}

/// Chooses the collation as GNU R's `Scollate` does after the locale changes: no ICU collator
/// when the `LC_ALL` environment variable (`LC_COLLATE` when `LC_ALL` is unset or empty) is
/// exactly `C`, or when the collation locale is `C`; otherwise ICU's collator for that locale.
/// It runs wherever the session sets `LC_ALL` or `LC_COLLATE`, which is where GNU R calls
/// `resetICUcollator`, and so drops what `icuSetCollate` set.
pub fn refresh_collation() {
    let variable = std::env::var_os("LC_ALL")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("LC_COLLATE"));
    let locale = icu_locale_name();
    let collation = if variable.is_some_and(|value| value == "C") || locale == "C" {
        without_collator()
    } else {
        icu_collation(&locale, CollatorOptions::default())
    };
    install(collation);
}

/// GNU R's `getLocale`: `R_ICU_LOCALE` when it is set, else the C library's collation locale.
fn icu_locale_name() -> String {
    if let Some(name) = std::env::var_os("R_ICU_LOCALE").filter(|value| !value.is_empty()) {
        return name.to_string_lossy().into_owned();
    }
    collation_locale()
}

fn collation_locale() -> String {
    // SAFETY: querying with a null locale only reads the current setting.
    let current = unsafe { libc::setlocale(libc::LC_COLLATE, std::ptr::null()) };
    if current.is_null() {
        return "C".to_owned();
    }
    // SAFETY: a non-null result is a NUL-terminated string the C library owns.
    let name = unsafe { std::ffi::CStr::from_ptr(current) };
    match name.to_string_lossy() {
        name if name == "POSIX" => "C".to_owned(),
        name => name.into_owned(),
    }
}

/// The collation without an ICU collator: `strcoll`, which orders by code point in the `C` and
/// `C.UTF-8` locales.
fn without_collator() -> Collation {
    let locale = collation_locale();
    if locale == "C" || locale.starts_with("C.") {
        Collation::CodePoint { ascii: false }
    } else {
        Collation::Strcoll
    }
}

fn strcoll(left: &str, right: &str) -> Ordering {
    let text = |text: &str| {
        let bytes =
            values::unescape_invalid_bytes(text).unwrap_or_else(|| text.as_bytes().to_vec());
        // An R string holds no NUL, so this keeps every byte.
        std::ffi::CString::new(bytes).unwrap_or_default()
    };
    let (left, right) = (text(left), text(right));
    // SAFETY: both are NUL-terminated strings that outlive the call.
    unsafe { libc::strcoll(left.as_ptr(), right.as_ptr()) }.cmp(&0)
}

/// The ICU collation for a POSIX or ICU locale name such as `de_DE.UTF-8` or
/// `de@collation=phonebook`. A name ICU4X has no data for, `C.UTF-8` among them, gets the root
/// collation, as ICU's locale fallback gives it.
fn icu_collation(name: &str, options: CollatorOptions) -> Collation {
    let (tag, label) = icu_locale_tag(name);
    let locale = Locale::try_from_str(&tag).ok();
    let preferences = locale
        .as_ref()
        .map(CollatorPreferences::from)
        .unwrap_or_default();
    let root_ascii_locale = locale
        .as_ref()
        .is_none_or(|locale| root_ascii_order(locale, &preferences));
    let label = if locale.is_some() && label != "und" {
        label
    } else {
        "root".to_owned()
    };
    build_icu_collation(label, preferences, options, root_ascii_locale)
}

fn build_icu_collation(
    locale: String,
    preferences: CollatorPreferences,
    options: CollatorOptions,
    root_ascii_locale: bool,
) -> Collation {
    let (collator, locale, preferences) = match CollatorBorrowed::try_new(preferences, options) {
        Ok(collator) => (collator, locale, preferences),
        Err(_) => {
            let root = CollatorPreferences::default();
            match CollatorBorrowed::try_new(root, options) {
                Ok(collator) => (collator, "root".to_owned(), root),
                Err(problem) => panic!("ICU4X's compiled root collation is missing: {problem}"),
            }
        }
    };
    let default_options = options.strength.is_none()
        && options.alternate_handling.is_none()
        && options.case_level.is_none()
        && preferences.case_first.is_none()
        && preferences.numeric_ordering.is_none();
    Collation::Icu(IcuCollation {
        collator,
        locale,
        preferences,
        options,
        root_ascii_locale,
        ascii_order: root_ascii_locale && default_options,
    })
}

/// A BCP 47 tag for a POSIX locale name, without its codeset (`.UTF-8`) and with ICU's
/// `@collation=` keyword as the `-u-co-` extension, and the name `icuGetCollate()` reports.
fn icu_locale_tag(name: &str) -> (String, String) {
    let (base, keywords) = name.split_once('@').unwrap_or((name, ""));
    let base = base.split_once('.').map_or(base, |(base, _)| base);
    let mut tag = base.replace('_', "-");
    let mut label = base.to_owned();
    let collation = keywords
        .split(';')
        .find_map(|keyword| keyword.strip_prefix("collation="));
    if let Some(collation) = collation {
        let short = match collation {
            "phonebook" => "phonebk",
            "traditional" => "trad",
            "dictionary" => "dict",
            "gb2312han" => "gb2312",
            other => other,
        };
        tag.push_str("-u-co-");
        tag.push_str(short);
        label.push_str("@collation=");
        label.push_str(collation);
    }
    (tag, label)
}

/// Whether the locale's collation orders printable ASCII exactly as the root collation does.
/// These languages have no CLDR tailoring that touches ASCII; the test
/// `ascii_collation_matches_icu` checks each against ICU4X's data.
fn root_ascii_order(locale: &Locale, preferences: &CollatorPreferences) -> bool {
    const ROOT_ORDER_LANGUAGES: &[&str] = &["und", "en", "de", "fr", "it", "nl", "pt"];
    ROOT_ORDER_LANGUAGES.contains(&locale.id.language.as_str())
        && locale.id.variants.is_empty()
        && preferences.collation_type.is_none()
}

fn is_printable_ascii(text: &str) -> bool {
    text.bytes().all(|byte| (b' '..=b'~').contains(&byte))
}

/// The root collation of two strings of printable ASCII. Each such character has one collation
/// element: its primary weight is unique up to case, its secondary weight is common, and its
/// tertiary weight puts lowercase first. So the primary weights decide unless the strings are
/// the same letters up to case, and then the first case difference decides.
fn ascii_collate(left: &[u8], right: &[u8]) -> Ordering {
    let mut case = Ordering::Equal;
    for (&left_byte, &right_byte) in left.iter().zip(right) {
        if left_byte == right_byte {
            continue;
        }
        let (left_rank, right_rank) = (
            ASCII_RANKS[left_byte as usize],
            ASCII_RANKS[right_byte as usize],
        );
        if left_rank != right_rank {
            return left_rank.cmp(&right_rank);
        }
        if case.is_eq() {
            case = left_byte
                .is_ascii_uppercase()
                .cmp(&right_byte.is_ascii_uppercase());
        }
    }
    left.len().cmp(&right.len()).then(case)
}

/// Byte keys, one per text, whose order is the session's collation, built once so a sort
/// compares bytes rather than collating each pair. No key is a proper prefix of another
/// followed only by zero bytes, so zero-padding a key keeps its order.
struct CollationKeys {
    bytes: Vec<u8>,
    starts: Vec<u32>,
}

impl CollationKeys {
    /// The keys of `texts` in [`ascii_collate`]'s order, when the session collates printable
    /// ASCII as the root collation does and every text is printable ASCII: the primary ranks,
    /// which are never zero, a zero, then 1 for each lowercase-or-caseless character and 2 for
    /// each uppercase one. Distinct texts have distinct keys.
    fn ascii<Text: AsRef<str>>(texts: &[Text]) -> Option<CollationKeys> {
        let eligible = MODE.load(AtomicOrdering::Relaxed) == ROOT_ASCII
            && texts.iter().all(|text| is_printable_ascii(text.as_ref()));
        if !eligible {
            return None;
        }
        let total: usize = texts.iter().map(|text| 2 * text.as_ref().len() + 1).sum();
        if u32::try_from(total).is_err() {
            return None;
        }
        let mut bytes = Vec::with_capacity(total);
        let mut starts = Vec::with_capacity(texts.len() + 1);
        for text in texts {
            let text = text.as_ref().as_bytes();
            starts.push(bytes.len() as u32);
            bytes.extend(text.iter().map(|&byte| ASCII_RANKS[byte as usize]));
            bytes.push(0);
            bytes.extend(text.iter().map(|&byte| byte.is_ascii_uppercase() as u8 + 1));
        }
        starts.push(bytes.len() as u32);
        Some(CollationKeys { bytes, starts })
    }

    fn key(&self, index: u32) -> &[u8] {
        let index = index as usize;
        &self.bytes[self.starts[index] as usize..self.starts[index + 1] as usize]
    }

    fn all(&self) -> Vec<&[u8]> {
        self.starts
            .windows(2)
            .map(|bounds| &self.bytes[bounds[0] as usize..bounds[1] as usize])
            .collect()
    }
}

/// Printable ASCII in the root collation's order, each letter's lowercase first.
const ROOT_ASCII_ORDER: &[u8] = b" _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789\
    aAbBcCdDeEfFgGhHiIjJkKlLmMnNoOpPqQrRsStTuUvVwWxXyYzZ";

/// One plus each printable ASCII character's rank in the root collation's primary order, which
/// a letter's two cases share; zero for any other byte.
const ASCII_RANKS: [u8; 128] = {
    let mut ranks = [0u8; 128];
    let mut position = 0;
    let mut rank = 0u8;
    while position < ROOT_ASCII_ORDER.len() {
        let byte = ROOT_ASCII_ORDER[position];
        if !byte.is_ascii_uppercase() {
            rank += 1;
        }
        ranks[byte as usize] = rank;
        position += 1;
    }
    ranks
};

/// `icuSetCollate(...)`: as GNU R's `do_ICUset`, `locale` opens a new collator (`"ASCII"`
/// compares bytes, `"none"` uses `strcoll`, `"default"` the collation locale) and the other
/// arguments set an attribute of the open collator. A name or value outside GNU R's table is
/// ignored, and so are `"default"` and the strength values, which that table spells with a
/// trailing space.
unsafe fn do_icu_set_collate(call: &Call<'_>) -> R<Sexp> {
    for arg in call.args {
        if arg.tag == nil() {
            return Err(error("all arguments must be named"));
        }
        let name = values::symbol_name(arg.tag);
        if arg.value.kind() != SexpType::String || length(arg.value) != 1 {
            return Err(error(format!("invalid '{name}' argument")));
        }
        // GNU R reads `NA` as the string "NA", which names no locale and no setting.
        let value = string_at(arg.value, 0).unwrap_or(Cow::Borrowed("NA"));
        if name == "locale" {
            install(match &*value {
                "ASCII" => Collation::CodePoint { ascii: true },
                "none" => without_collator(),
                "default" => icu_collation(&icu_locale_name(), CollatorOptions::default()),
                other => icu_collation(other, CollatorOptions::default()),
            });
            continue;
        }
        let attribute = ATTRIBUTES.iter().find(|entry| entry.0 == name);
        let setting = ATTRIBUTES.iter().find(|entry| entry.0 == value);
        let (Some(&(_, attribute)), Some(&(_, setting))) = (attribute, setting) else {
            continue;
        };
        let current = COLLATION.read().unwrap_or_else(PoisonError::into_inner);
        let Collation::Icu(icu) = &*current else {
            continue;
        };
        let (mut preferences, mut options) = (icu.preferences, icu.options);
        match (attribute, setting) {
            (Attribute::Strength, Attribute::StrengthValue(strength)) => {
                options.strength = Some(strength)
            }
            // `ucol_setStrength` ignores a value that is not a strength.
            (Attribute::Strength, _) => continue,
            (Attribute::CaseFirst, Attribute::Off) => {
                preferences.case_first = Some(CollationCaseFirst::False)
            }
            (Attribute::CaseFirst, Attribute::Upper) => {
                preferences.case_first = Some(CollationCaseFirst::Upper)
            }
            (Attribute::CaseFirst, Attribute::Lower) => {
                preferences.case_first = Some(CollationCaseFirst::Lower)
            }
            (Attribute::AlternateHandling, Attribute::NonIgnorable) => {
                options.alternate_handling = Some(AlternateHandling::NonIgnorable)
            }
            (Attribute::AlternateHandling, Attribute::Shifted) => {
                options.alternate_handling = Some(AlternateHandling::Shifted)
            }
            (Attribute::CaseLevel, Attribute::On) => options.case_level = Some(CaseLevel::On),
            (Attribute::CaseLevel, Attribute::Off) => options.case_level = Some(CaseLevel::Off),
            // ICU ignores the Hiragana quaternary mode, and ICU4X always normalizes, which gives
            // ICU's order with normalization on, and with it off for all text in FCD form.
            (
                Attribute::HiraganaQuaternary | Attribute::Normalization,
                Attribute::On | Attribute::Off,
            ) => continue,
            (Attribute::FrenchCollation, Attribute::On | Attribute::Off) => {
                return Err(error(
                    "ry's collator does not support the 'french_collation' attribute",
                ));
            }
            _ => return Err(error("failed to set ICU collator attribute")),
        }
        let (locale, root_ascii_locale) = (icu.locale.clone(), icu.root_ascii_locale);
        drop(current);
        install(build_icu_collation(
            locale,
            preferences,
            options,
            root_ascii_locale,
        ));
    }
    Ok(nil())
}

/// `icuGetCollate(type)`: the locale of the open collator, `"ASCII"`, or `"ICU not in use"`.
unsafe fn do_icu_get_collate(_call: &Call<'_>) -> R<Sexp> {
    let label = match &*collator().0 {
        Collation::CodePoint { ascii: true } => "ASCII".to_owned(),
        Collation::CodePoint { ascii: false } | Collation::Strcoll => "ICU not in use".to_owned(),
        Collation::Icu(icu) => icu.locale.clone(),
    };
    values::scalar_string(&label)
}

/// GNU R's `ATtable`: the names and values `icuSetCollate` accepts. Its `"default "` entry is
/// left out because `UCOL_DEFAULT` is negative, which `do_ICUset` treats as not found.
#[derive(Clone, Copy)]
enum Attribute {
    CaseFirst,
    Upper,
    Lower,
    Strength,
    StrengthValue(Strength),
    FrenchCollation,
    On,
    Off,
    Normalization,
    AlternateHandling,
    NonIgnorable,
    Shifted,
    CaseLevel,
    HiraganaQuaternary,
}

const ATTRIBUTES: &[(&str, Attribute)] = &[
    ("case_first", Attribute::CaseFirst),
    ("upper", Attribute::Upper),
    ("lower", Attribute::Lower),
    ("strength", Attribute::Strength),
    ("primary ", Attribute::StrengthValue(Strength::Primary)),
    ("secondary ", Attribute::StrengthValue(Strength::Secondary)),
    ("tertiary ", Attribute::StrengthValue(Strength::Tertiary)),
    (
        "quaternary ",
        Attribute::StrengthValue(Strength::Quaternary),
    ),
    ("identical ", Attribute::StrengthValue(Strength::Identical)),
    ("french_collation", Attribute::FrenchCollation),
    ("on", Attribute::On),
    ("off", Attribute::Off),
    ("normalization", Attribute::Normalization),
    ("alternate_handling", Attribute::AlternateHandling),
    ("non_ignorable", Attribute::NonIgnorable),
    ("shifted", Attribute::Shifted),
    ("case_level", Attribute::CaseLevel),
    ("hiragana_quaternary", Attribute::HiraganaQuaternary),
];

/// The comparison `identical()` makes, with one field per argument of the R function.
#[derive(Clone, Copy, Debug)]
pub struct IdenticalFlags {
    pub num_eq: bool,
    pub single_na: bool,
    pub attributes_as_set: bool,
    pub ignore_bytecode: bool,
    pub ignore_environment: bool,
    pub ignore_srcref: bool,
    pub external_pointer_as_reference: bool,
}

impl Default for IdenticalFlags {
    fn default() -> IdenticalFlags {
        IdenticalFlags {
            num_eq: true,
            single_na: true,
            attributes_as_set: true,
            ignore_bytecode: true,
            ignore_environment: false,
            ignore_srcref: true,
            external_pointer_as_reference: false,
        }
    }
}

/// Whether `x` and `y` are identical in the sense of R's `identical()`.
pub unsafe fn identical(x: Sexp, y: Sexp, flags: &IdenticalFlags) -> bool {
    identical_skipping(x, y, flags, &[])
}

pub(crate) unsafe fn do_sort(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let decreasing = flag(call.arg(1), "'decreasing' must be TRUE or FALSE")?;
    if x == nil() {
        return Ok(nil());
    }
    check_sortable(x)?;
    // Strings are compared through their characters, which a string declared as bytes lacks.
    if length(x) > 1 {
        values::refuse_bytes(x)?;
    }
    let result = values::duplicate(x)?;
    values::clear_attributes(result);
    match result.kind() {
        SexpType::Logical | SexpType::Integer => {
            // NA is the smallest integer, which is where GNU R's integer sort leaves it.
            let items = values::ints_mut(result);
            if decreasing {
                items.sort_unstable_by(|left, right| right.cmp(left));
            } else {
                items.sort_unstable();
            }
        }
        SexpType::Real => {
            // Plain comparisons, so a NaN stays where it is, as in GNU R. The R wrappers remove
            // NAs before they get here.
            let items = values::reals_mut(result);
            // Without NaN or a negative zero, elements that compare equal are bitwise equal, so
            // any correct sort gives the shell sort's exact result, and pattern-defeating
            // quicksort is several times faster.
            if !items
                .iter()
                .any(|item| item.is_nan() || (*item == 0.0 && item.is_sign_negative()))
            {
                if decreasing {
                    items.sort_unstable_by(|left, right| right.total_cmp(left));
                } else {
                    items.sort_unstable_by(f64::total_cmp);
                }
            } else if decreasing {
                shell_sort(items, |left, right| left < right);
            } else {
                shell_sort(items, |left, right| left > right);
            }
        }
        SexpType::Complex => {
            if decreasing {
                shell_sort(values::complexes_mut(result), complex_less);
            } else {
                shell_sort(values::complexes_mut(result), |left, right| {
                    complex_less(right, left)
                });
            }
        }
        SexpType::String => {
            // Sorting the strings' collation ranks compares each distinct string once. `NA`
            // sorts last, or first when decreasing, as the reversed comparison put it.
            let source = values::materialize(x)?;
            let ranks = Column::Ranks(string_ranks(source, TextOrder::Collation));
            let Some(keys) = sort_keys(&ranks, decreasing, !decreasing) else {
                unreachable!("string ranks always have radix keys");
            };
            let mut order = vec![0u32; length(x)];
            radix_order(&mut [keys], &mut order);
            let Column::Ranks(ranks) = ranks else {
                unreachable!("the column was built from ranks");
            };
            // GNU R sorts strings with a shell sort, which is not stable, so where the collation
            // calls two different strings equal (`"é"` and `"e\u{301}"`) only that sort puts
            // them where GNU R does.
            let tied = order.windows(2).any(|pair| {
                let (left, right) = (pair[0] as usize, pair[1] as usize);
                ranks[left] == ranks[right] && source.element(left) != source.element(right)
            });
            if tied {
                let key = |row: u32| match ranks[row as usize] {
                    0 => u32::MAX,
                    rank => rank,
                };
                order = (0..length(x) as u32).collect();
                if decreasing {
                    shell_sort(&mut order, |left, right| key(left) < key(right));
                } else {
                    shell_sort(&mut order, |left, right| key(left) > key(right));
                }
            }
            for (index, &row) in order.iter().enumerate() {
                values::set_elt(result, index, source.element(row as usize));
            }
        }
        _ => {}
    }
    Ok(result)
}

/// `psort(x, partial)`: rearranges `x` so that each position in `partial` holds the element a
/// full sort would put there, with smaller elements before it and larger ones after.
unsafe fn do_psort(call: &Call<'_>) -> R<Sexp> {
    let x = values::materialize(call.arg(0))?;
    check_sortable(x)?;
    let count = length(x);
    let partial = coerce_vector(call.arg(1), SexpType::Real)?;
    let mut positions = Vec::with_capacity(length(partial));
    for &position in values::reals(partial) {
        if !(position >= 1.0 && position <= count as f64) {
            return Err(error(format!(
                "'partial' index {} outside bounds",
                position as i64
            )));
        }
        positions.push(position as usize - 1);
    }
    positions.sort_unstable();
    // Numbers are selected in place on a copy of the values; NA and NaN compare equal to each
    // other and after every number, as `rcmp(nalast = TRUE)` orders them.
    match x.kind() {
        SexpType::Real => {
            let result = values::alloc_uninit(SexpType::Real, count)?;
            let items = values::reals_mut(result);
            items.copy_from_slice(values::reals(x));
            let compare = |left: f64, right: f64| match (left.is_nan(), right.is_nan()) {
                (false, false) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
            };
            partial_sort(items, 0, count as isize - 1, &positions, &compare);
            return Ok(result);
        }
        SexpType::Integer | SexpType::Logical => {
            let result = values::alloc_uninit(x.kind(), count)?;
            let items = values::ints_mut(result);
            items.copy_from_slice(values::ints(x));
            // `NA_integer_` is the smallest `i32`, so flipping it above every number orders it last.
            let compare = |left: i32, right: i32| left.wrapping_sub(1).cmp(&right.wrapping_sub(1));
            partial_sort(items, 0, count as isize - 1, &positions, &compare);
            return Ok(result);
        }
        _ => {}
    }
    let column = column(x).ok_or_else(|| error("only atomic vectors can be sorted"))?;
    let mut order: Vec<usize> = (0..count).collect();
    let compare = |left: usize, right: usize| column.compare(left, right, true);
    partial_sort(&mut order, 0, count as isize - 1, &positions, &compare);
    select_elements(x, &order)
}

/// `qsort(x, index.return)`: Singleton's quicksort, whose arrangement of ties `index.return`
/// exposes.
unsafe fn do_qsort(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let numeric = matches!(
        x.kind(),
        SexpType::Logical | SexpType::Integer | SexpType::Real
    ) && !values::inherits(x, "factor");
    if !numeric {
        return Err(error("argument is not a numeric vector"));
    }
    let index_return = flag(call.arg(1), "'index.return' must be TRUE or FALSE")?;
    let result = values::duplicate(x)?;
    values::clear_attributes(result);
    let mut index: Vec<i32> = (1..=length(result) as i32).collect();
    match result.kind() {
        SexpType::Real => singleton_sort(values::reals_mut(result), &mut index),
        _ => singleton_sort(values::ints_mut(result), &mut index),
    }
    if !index_return {
        return Ok(result);
    }
    let index = values::integer_vector(&index)?;
    values::named_list(&[("x", result), ("ix", index)])
}

/// `sorted_fpass(x, decreasing, na.last)`: whether `x` is known to be in the wanted order, so
/// `sort.int`, `order` and `sort.list` return it (or `seq_along(x)`) without sorting. The flags
/// go through GNU R's `asInteger`, whose coercion warnings `sort.int` reports.
///
/// The check is GNU R's `fastpass_sortcheck`: an ALTREP vector answers from its sortedness
/// metadata, and a plain integer vector wanted increasing is scanned for non-decreasing order
/// without `NA`.
///
/// GNU R's `sort` and `order` put that metadata on every numeric result, which ry's do not,
/// because a wrapper makes every later read from R code slower. So ry scans the other plain
/// vectors too, but only for strict order: with distinct values every sorting method gives the
/// identity permutation, where a tie could come out of `method = "quick"` permuted.
unsafe fn do_sorted_fpass(call: &Call<'_>) -> R<Sexp> {
    for flag in [call.arg(1), call.arg(2)] {
        let mut warnings = CoercionWarnings::default();
        if length(flag) > 0 {
            match flag.kind() {
                SexpType::String => {
                    integer_from_string(heap::handle::Gc::adopt(flag.element(0)), &mut warnings)?;
                }
                SexpType::Real => warnings.integer_range = real_to_int(values::reals(flag)[0]).1,
                SexpType::Complex => {
                    let value = values::complexes(flag)[0];
                    warnings.integer_range = real_to_int(value.r).1;
                    warnings.imaginary = !is_na_real(value.r) && value.i != 0.0;
                }
                _ => {}
            }
        }
        warnings.report()?;
    }
    let decreasing = values::as_integer_scalar(call.arg(1));
    let na_last = values::as_integer_scalar(call.arg(2));
    let wanted = match (decreasing, na_last) {
        (NA_INTEGER, _) => return values::scalar_bool(false),
        (0, 0) => SORTED_INCR_NA_FIRST,
        (0, _) => SORTED_INCR,
        (_, 0) => SORTED_DECR_NA_FIRST,
        _ => SORTED_DECR,
    };
    let x = call.arg(0);
    if !matches!(x.kind(), SexpType::Integer | SexpType::Real) || length(x) == 0 {
        return values::scalar_bool(false);
    }
    if x.is_altrep() {
        let (sorted, no_na) = (crate::interp().hooks.altrep_sortedness)(x)?;
        let known = matches!(sorted, -2 | -1 | 1 | 2);
        return values::scalar_bool(known && (sorted == wanted || (no_na && sorted * wanted > 0)));
    }
    // GNU R scans only an integer vector wanted increasing, where ties pass.
    let ties = x.kind() == SexpType::Integer && wanted > 0;
    values::scalar_bool(column(x).is_some_and(|x| in_order(&x, wanted < 0, !ties)))
}

/// Whether an integer or double column without `NA` or NaN is non-decreasing, or
/// non-increasing when `decreasing`, and strictly so when `strict`.
fn in_order(x: &Column<'_>, decreasing: bool, strict: bool) -> bool {
    fn ordered<T: PartialOrd + Copy>(values: &[T], decreasing: bool, strict: bool) -> bool {
        match (decreasing, strict) {
            (false, false) => adjacent_pairs_all(values, |left, right| left <= right),
            (false, true) => adjacent_pairs_all(values, |left, right| left < right),
            (true, false) => adjacent_pairs_all(values, |left, right| left >= right),
            (true, true) => adjacent_pairs_all(values, |left, right| left > right),
        }
    }
    match *x {
        // The order check stops at the first pair out of order, so it runs first.
        Column::Integer(values) => {
            ordered(values, decreasing, strict) && !values.contains(&NA_INTEGER)
        }
        // A NaN fails every comparison, so only a lone one could pass as in order.
        Column::Real(values) => {
            !values.first().is_some_and(|value| value.is_nan())
                && ordered(values, decreasing, strict)
        }
        _ => false,
    }
}

/// Whether `ordered(values[i], values[i + 1])` holds for every adjacent pair. A block of pairs is
/// tested without an early exit inside it, so the comparisons vectorize; an early exit per pair,
/// as `is_sorted_by` takes, compiles to one compare and branch per element.
pub(crate) fn adjacent_pairs_all<T: Copy>(values: &[T], ordered: impl Fn(T, T) -> bool) -> bool {
    const BLOCK: usize = 16;
    let Some(last) = values.len().checked_sub(1) else {
        return true;
    };
    let mut lefts = values[..last].chunks_exact(BLOCK);
    let mut rights = values[1..].chunks_exact(BLOCK);
    for (left, right) in (&mut lefts).zip(&mut rights) {
        let all = left
            .iter()
            .zip(right)
            .fold(true, |all, (&left, &right)| all & ordered(left, right));
        if !all {
            return false;
        }
    }
    lefts
        .remainder()
        .iter()
        .zip(rights.remainder())
        .all(|(&left, &right)| ordered(left, right))
}

const SORTED_DECR_NA_FIRST: i32 = -2;
const SORTED_DECR: i32 = -1;
const SORTED_INCR: i32 = 1;
const SORTED_INCR_NA_FIRST: i32 = 2;

/// `order(na.last, decreasing, ...)`: the stable permutation that sorts the keys
/// lexicographically.
pub(crate) unsafe fn do_order(call: &Call<'_>) -> R<Sexp> {
    let na_last = match as_logical_scalar(call.arg(0)) {
        NA_LOGICAL => return Err(error("invalid 'na.last' value")),
        value => value != 0,
    };
    let decreasing = flag(call.arg(1), "'decreasing' must be TRUE or FALSE")?;
    let keys: Vec<Sexp> = call.args.iter().skip(2).map(|arg| arg.value).collect();
    let Some(&first) = keys.first() else {
        return Ok(nil());
    };
    let count = length(first);
    for (position, &key) in keys.iter().enumerate() {
        if !(key.kind().is_atomic() || matches!(key.kind(), SexpType::List | SexpType::Expression))
        {
            return Err(error(format!("argument {} is not a vector", position + 1)));
        }
        if length(key) != count {
            return Err(error("argument lengths differ"));
        }
    }
    let keys = keys
        .iter()
        .map(|&key| values::materialize(key))
        .collect::<R<Vec<Sexp>>>()?;
    if count > 1 {
        for &key in &keys {
            values::refuse_bytes(key)?;
        }
    }
    // GNU R's `orderVector1` and `listgreater` have no raw case. Strings become their ranks
    // in collation order, so every key but a complex one sorts as integers by the radix sort.
    let columns: Vec<Option<Column<'_>>> = keys
        .iter()
        .map(|&key| match key.kind() {
            SexpType::Raw => None,
            SexpType::String => Some(Column::Ranks(string_ranks(key, TextOrder::Collation))),
            _ => column(key),
        })
        .collect();
    let routine = if keys.len() == 1 {
        "orderVector1"
    } else {
        "listgreater"
    };
    if keys.len() == 1 && columns[0].is_none() {
        return Err(unimplemented_type(first, routine));
    }
    let radix_keys: Option<Vec<SortKeys>> = columns
        .iter()
        .map(|column| {
            column
                .as_ref()
                .and_then(|column| sort_keys(column, decreasing, na_last))
        })
        .collect();
    if let Some(mut radix_keys) = radix_keys {
        let permutation = alloc(SexpType::Integer, count)?;
        let ints = values::ints_mut(permutation);
        let order: &mut [u32] =
            std::slice::from_raw_parts_mut(ints.as_mut_ptr().cast::<u32>(), ints.len());
        radix_order(&mut radix_keys, order);
        for slot in order.iter_mut() {
            *slot += 1;
        }
        return Ok(permutation);
    }
    let directions = vec![decreasing; columns.len()];
    let unsupported = Cell::new(None);
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by(|&left, &right| {
        compare_rows(&columns, &directions, left, right, na_last, &unsupported)
    });
    if let Some(position) = unsupported.get() {
        return Err(unimplemented_type(keys[position], routine));
    }
    one_based(&order)
}

/// `radixsort(na.last, decreasing, retgrp, sortstr, ...)`, which `order(method = "radix")` and
/// `sort(method = "radix")` call. `na.last = NA` drops the rows with an NA in any key, and
/// `retgrp` adds the group boundaries of equal rows.
pub(crate) unsafe fn do_radixsort(call: &Call<'_>) -> R<Sexp> {
    let na_last = as_logical_scalar(call.arg(0));
    let return_groups = as_logical_scalar(call.arg(2)) == 1;
    let sort_strings = as_logical_scalar(call.arg(3)) != 0;
    let keys: Vec<Sexp> = call.args.iter().skip(4).map(|arg| arg.value).collect();
    if keys.is_empty() {
        return Ok(nil());
    }
    let mut materialized = Vec::with_capacity(keys.len());
    for (position, &key) in keys.iter().enumerate() {
        if !key.kind().is_vector() {
            return Err(error(format!("argument {} is not a vector", position + 1)));
        }
        materialized.push(values::materialize(key)?);
    }
    let count = length(keys[0]);
    if keys.iter().any(|&key| length(key) != count) {
        return Err(error("argument lengths differ"));
    }
    let decreasing = coerce_vector(call.arg(1), SexpType::Logical)?;
    if length(decreasing) != keys.len() {
        return Err(error(
            "length(decreasing) must match the number of order arguments",
        ));
    }
    let directions = values::ints(decreasing)
        .iter()
        .map(|&value| match value {
            NA_LOGICAL => Err(error("'decreasing' elements must be TRUE or FALSE")),
            value => Ok(value != 0),
        })
        .collect::<R<Vec<bool>>>()?;
    // Input already in order, as `order(sort(x))` gives it, is the identity permutation, since
    // the radix sort is stable and keeps signed zeros as ties. Input strictly in the opposite
    // order has no ties to keep stable, so it reverses, as GNU R's radix sort does.
    if let [key] = materialized[..]
        && !return_groups
        && matches!(key.kind(), SexpType::Integer | SexpType::Real)
        && !key.is_altrep()
        && let Some(key) = column(key)
    {
        let reversed = if in_order(&key, directions[0], false) {
            Some(false)
        } else if count > 1 && in_order(&key, !directions[0], true) {
            Some(true)
        } else {
            None
        };
        if let Some(reversed) = reversed {
            let permutation = alloc(SexpType::Integer, count)?;
            let rows = values::ints_mut(permutation);
            if reversed {
                for (slot, row) in rows.iter_mut().zip((1..=count as i32).rev()) {
                    *slot = row;
                }
            } else {
                for (slot, row) in rows.iter_mut().zip(1..) {
                    *slot = row;
                }
            }
            return Ok(permutation);
        }
    }
    let columns: Vec<Option<Column<'_>>> = materialized
        .iter()
        .map(|&key| radix_column(key, sort_strings))
        .collect();
    if columns[0].is_none() {
        return Err(error(format!(
            "First arg is type '{}', not yet supported",
            keys[0].kind().name()
        )));
    }
    let nas_last = na_last == 1;
    let mut radix_keys: Option<Vec<SortKeys>> = columns
        .iter()
        .zip(&directions)
        .map(|(column, &decreasing)| {
            column
                .as_ref()
                .and_then(|column| sort_keys(column, decreasing, nas_last))
        })
        .collect();
    // The order is computed 0-based in the result vector itself, which saves a copy of the
    // largest buffer: `u32` and `i32` share a layout, and every row index fits in both.
    let permutation = alloc(SexpType::Integer, count)?;
    let ints = values::ints_mut(permutation);
    let order: &mut [u32] =
        std::slice::from_raw_parts_mut(ints.as_mut_ptr().cast::<u32>(), ints.len());
    for key in radix_keys.iter_mut().flatten() {
        if let SortKeys::Appearance(key) = key {
            key.nas_unsorted = na_last == NA_LOGICAL;
        }
    }
    let ends = match radix_keys {
        Some(mut keys) if return_groups => Some(radix_groups(&mut keys, order)),
        Some(mut keys) => {
            radix_order(&mut keys, order);
            None
        }
        // A key the radix sort cannot read (a complex or raw vector after the first key) is reported
        // only when rows tie on every earlier key, so it needs the comparison sort.
        None => {
            let unsupported = Cell::new(None);
            fill_identity(order);
            order.sort_by(|&left, &right| {
                compare_rows(
                    &columns,
                    &directions,
                    left as usize,
                    right as usize,
                    nas_last,
                    &unsupported,
                )
            });
            if let Some(position) = unsupported.get() {
                return Err(error(format!(
                    "Arg {} is type '{}', not yet supported",
                    position + 1,
                    keys[position].kind().name()
                )));
            }
            return_groups.then(|| {
                let mut ends = Vec::new();
                for position in 1..=order.len() {
                    if position == order.len()
                        || compare_rows(
                            &columns,
                            &directions,
                            order[position - 1] as usize,
                            order[position] as usize,
                            nas_last,
                            &unsupported,
                        ) != Ordering::Equal
                    {
                        ends.push(position as i32);
                    }
                }
                ends
            })
        }
    };
    // A string key kept in order of appearance has an NA where its byte rank is 0. GNU R's
    // `csorted` drops no NA from the first key unless every entry is NA, and then drops every
    // row; a later key drops its NA rows like any other key.
    let string_na = |column: &Column<'_>, index: usize| matches!(column, Column::Grouped(codes, byte_ranks) if byte_ranks[codes[index] as usize] == 0);
    let only_missing_strings = na_last == NA_LOGICAL
        && matches!(columns.first(), Some(Some(first @ Column::Grouped(..)))
            if (0..count).all(|index| string_na(first, index)));
    let missing = |index: usize| {
        only_missing_strings
            || columns.iter().enumerate().any(|(position, column)| {
                column.as_ref().is_some_and(|column| {
                    column.is_na(index) || position > 0 && string_na(column, index)
                })
            })
    };
    // `na.last = NA` is the default of `sort`, and most keys have no NA to drop.
    if na_last != NA_LOGICAL || !(0..count).any(missing) {
        for slot in order.iter_mut() {
            *slot += 1;
        }
        return with_groups(permutation, ends);
    }
    // `na.last = NA` drops the rows with an NA key, except that with groups they stay as 0.
    let mut result: Vec<i32> = order
        .iter()
        .map(|&index| {
            if missing(index as usize) {
                0
            } else {
                index as i32 + 1
            }
        })
        .collect();
    if ends.is_none() {
        result.retain(|&index| index != 0);
    }
    with_groups(values::integer_vector(&result)?, ends)
}

/// `permutation` with the `ends`, `maxgrpn` and class that `radixsort(retgrp = TRUE)` adds.
unsafe fn with_groups(permutation: Sexp, ends: Option<Vec<i32>>) -> R<Sexp> {
    if let Some(ends) = ends {
        let largest = ends
            .iter()
            .scan(0, |previous, &end| {
                let size = end - *previous;
                *previous = end;
                Some(size)
            })
            .max()
            .unwrap_or(NA_INTEGER);
        let ends_vector = values::integer_vector(&ends)?;
        values::set_attr(permutation, values::install("ends")?, ends_vector)?;
        let largest = values::scalar_integer(largest)?;
        values::set_attr(permutation, values::install("maxgrpn")?, largest)?;
        let class = values::string_vector(&["grouping", "integer"])?;
        values::set_attr(permutation, symbols().class, class)?;
    }
    Ok(permutation)
}

/// `rank(x, length, ties)` for the tie methods GNU R computes in C: `"average"`, `"min"` and
/// `"max"`.
unsafe fn do_rank(call: &Call<'_>) -> R<Sexp> {
    let x = values::materialize(call.arg(0))?;
    // A classed raw vector is ranked by its bytes, which is what `xtfrm` and so `order` and
    // `sort` rely on; GNU R refuses only a bare raw vector.
    if x.kind() == SexpType::Raw && !x.is_object() {
        return Err(error("raw vectors cannot be sorted"));
    }
    if !matches!(
        x.kind(),
        SexpType::Logical
            | SexpType::Integer
            | SexpType::Real
            | SexpType::Complex
            | SexpType::Raw
            | SexpType::String
    ) {
        return Err(unimplemented_type(x, "greater"));
    }
    let requested = values::as_real_scalar(call.arg(1));
    if requested.is_nan() || requested < 0.0 {
        return Err(error("invalid 'length(xx)' value"));
    }
    let count = (requested as usize).min(length(x));
    let ties = match values::first_string(call.arg(2)).as_deref() {
        Some("average") => Ties::Average,
        Some("min") => Ties::Min,
        Some("max") => Ties::Max,
        _ => {
            return Err(error(
                "invalid ties.method for rank() [should never happen]",
            ));
        }
    };
    // The rank shared by the rows in sorted positions `start..=end`.
    let rank_of = |start: usize, end: usize| match ties {
        Ties::Average => (start + end + 2) as f64 / 2.0,
        Ties::Min => (start + 1) as f64,
        Ties::Max => (end + 1) as f64,
    };
    let mut ranks = vec![0.0; count];
    let rank_column = match x.kind() {
        SexpType::Logical | SexpType::Integer | SexpType::Real => column(x),
        SexpType::String if count == length(x) => {
            Some(Column::Ranks(string_ranks(x, TextOrder::Collation)))
        }
        _ => None,
    };
    let keys = rank_column
        .and_then(|column| complete_prefix(column, count))
        .and_then(|column| sort_keys(&column, false, true));
    if let Some(keys) = keys {
        rank_by_keys(keys, &rank_of, &mut ranks);
    } else {
        let column = column(x).ok_or_else(|| unimplemented_type(x, "greater"))?;
        let mut order: Vec<usize> = (0..count).collect();
        order.sort_by(|&left, &right| column.compare(left, right, true));
        let mut start = 0;
        while start < count {
            let mut end = start;
            while end + 1 < count
                && column.compare(order[start], order[end + 1], true) == Ordering::Equal
            {
                end += 1;
            }
            for &row in &order[start..=end] {
                ranks[row] = rank_of(start, end);
            }
            start = end + 1;
        }
    }
    if ties == Ties::Average {
        values::real_vector(&ranks)
    } else {
        let ranks: Vec<i32> = ranks.iter().map(|&rank| rank as i32).collect();
        values::integer_vector(&ranks)
    }
}

/// The first `count` elements of `x`, or `None` when one is missing, which only the general
/// comparison orders.
fn complete_prefix(x: Column<'_>, count: usize) -> Option<Column<'_>> {
    match x {
        Column::Integer(data) => {
            let data = data.get(..count)?;
            (!data.contains(&NA_INTEGER)).then_some(Column::Integer(data))
        }
        Column::Real(data) => {
            let data = data.get(..count)?;
            (!data.iter().any(|value| value.is_nan())).then_some(Column::Real(data))
        }
        Column::Ranks(ranks) => {
            (ranks.len() == count && !ranks.contains(&0)).then_some(Column::Ranks(ranks))
        }
        _ => None,
    }
}

/// Writes each row's rank by `keys` into `ranks`, where `rank_of` maps the sorted positions a
/// run of tied rows takes to their shared rank. A key whose range is no wider than the rows
/// (string ranks, small integers) needs no sort: counting each key gives the positions its
/// rows take.
fn rank_by_keys(mut keys: SortKeys, rank_of: &dyn Fn(usize, usize) -> f64, ranks: &mut [f64]) {
    if ranks.is_empty() {
        return;
    }
    if let SortKeys::Narrow(keys) = &keys
        && keys.high - keys.low < ranks.len() as u64
    {
        let low = keys.low as u32;
        let mut ends = vec![0u32; (keys.high - keys.low) as usize + 1];
        for &key in &keys.values {
            ends[(key - low) as usize] += 1;
        }
        let mut total = 0;
        for end in &mut ends {
            total += *end;
            *end = total;
        }
        let shared: Vec<f64> = ends
            .iter()
            .scan(0, |start, &end| {
                let rank = rank_of(*start as usize, end.saturating_sub(1) as usize);
                *start = end;
                Some(rank)
            })
            .collect();
        for (rank, &key) in ranks.iter_mut().zip(&keys.values) {
            *rank = shared[(key - low) as usize];
        }
        return;
    }
    let mut order = vec![0u32; ranks.len()];
    radix_order(std::slice::from_mut(&mut keys), &mut order);
    let mut start = 0;
    while start < order.len() {
        let mut end = start;
        while end + 1 < order.len() && keys.equal(order[start] as usize, order[end + 1] as usize) {
            end += 1;
        }
        let rank = rank_of(start, end);
        for &row in &order[start..=end] {
            ranks[row as usize] = rank;
        }
        start = end + 1;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ties {
    Average,
    Min,
    Max,
}

pub(crate) unsafe fn do_unique(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    if x == nil() {
        return Ok(nil());
    }
    let from_last = flag(call.arg(2), "'fromLast' must be TRUE or FALSE")?;
    let duplicated = duplicated_elements(x, call.arg(1), from_last, false, "unique")?;
    let kept = duplicated.len - duplicated.count();
    crate::subset::select_positions(x, duplicated.clear_positions(), kept)
}

unsafe fn do_duplicated(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let from_last = flag(call.arg(2), "'fromLast' must be TRUE or FALSE")?;
    if x == nil() {
        return alloc(SexpType::Logical, 0);
    }
    let duplicated = duplicated_elements(x, call.arg(1), from_last, false, "duplicated")?;
    let result = values::alloc_uninit(SexpType::Logical, duplicated.len)?;
    let flags = values::ints_mut(result).chunks_mut(64);
    for (chunk, &word) in flags.zip(&duplicated.words) {
        for (bit, slot) in chunk.iter_mut().enumerate() {
            *slot = (word >> bit & 1) as i32;
        }
    }
    Ok(result)
}

unsafe fn do_any_duplicated(call: &Call<'_>) -> R<Sexp> {
    let x = call.arg(0);
    let from_last = flag(call.arg(2), "'fromLast' must be TRUE or FALSE")?;
    if x == nil() {
        return values::scalar_integer(0);
    }
    let duplicated = duplicated_elements(x, call.arg(1), from_last, true, "anyDuplicated")?;
    let found = if from_last {
        duplicated.last()
    } else {
        duplicated.first()
    };
    values::scalar_integer(found.map_or(0, |index| index as i32 + 1))
}

/// `match(x, table, nomatch, incomparables)`. Both sides are converted to a common type first:
/// character when either is character or of a later type (lists and raw vectors included),
/// otherwise the higher of the two numeric types. Factors match by their labels, and other
/// classed objects by what `mtfrm()` makes of them.
pub(crate) unsafe fn do_match(call: &Call<'_>) -> R<Sexp> {
    let x = match_operand(call.arg(0), call)?;
    let table = match_operand(call.arg(1), call)?;
    let nomatch = values::as_integer_scalar_warning(call.arg(2))?;
    let count = length(x);
    if count == 0 {
        return alloc(SexpType::Integer, 0);
    }
    if length(table) == 0 {
        return values::integer_vector(&vec![nomatch; count]);
    }
    let string_code = SexpType::String.code();
    let kind = if x.kind().code() >= string_code || table.kind().code() >= string_code {
        SexpType::String
    } else if x.kind().code() >= table.kind().code() {
        x.kind()
    } else {
        table.kind()
    };
    let x = values::materialize(coerce_vector(x, kind)?)?;
    let table = values::materialize(coerce_vector(table, kind)?)?;
    let incomparables = incomparable_keys(call.arg(3), kind)?;
    if incomparables.is_none()
        && let (Some(table_keys), Some(x_keys)) = (plain_keys(table), plain_keys(x))
        && plain_kinds_match(table_keys, x_keys)
    {
        let result = values::alloc_uninit(SexpType::Integer, count)?;
        let positions = values::ints_mut(result);
        if match_positions(table_keys, x_keys, positions) {
            if nomatch != 0 {
                for position in positions.iter_mut().filter(|position| **position == 0) {
                    *position = nomatch;
                }
            }
            return Ok(result);
        }
    }
    let mut positions: FxHashMap<Key<'_>, i32> = FxHashMap::default();
    for (index, key) in element_keys(table).into_iter().enumerate() {
        positions.entry(key).or_insert(index as i32 + 1);
    }
    let result: Vec<i32> = element_keys(x)
        .into_iter()
        .map(|key| {
            if incomparables
                .as_ref()
                .is_some_and(|keys| keys.contains(&key))
            {
                return nomatch;
            }
            positions.get(&key).copied().unwrap_or(nomatch)
        })
        .collect();
    values::integer_vector(&result)
}

/// `rowsum_matrix(x, group, ugroup, na.rm, snames)`: the column sums of `x` within each group,
/// one row per element of `ugroup`, named by `snames`.
unsafe fn do_rowsum_matrix(call: &Call<'_>) -> R<Sexp> {
    let x = values::materialize(call.arg(0))?;
    let (rows, columns) = match values::dims(x).as_deref() {
        Some(&[rows, columns]) => (rows, columns),
        _ => (length(x), 1),
    };
    let groups = group_positions(call)?;
    let result = alloc(x.kind(), groups.count * columns)?;
    for column in 0..columns {
        let input = column * rows..(column + 1) * rows;
        let output = column * groups.count..(column + 1) * groups.count;
        group_sums(call, x, input, result, output, &groups)?;
    }
    values::set_attr(
        result,
        symbols().dim,
        values::integer_vector(&[groups.count as i32, columns as i32])?,
    )?;
    let column_names = match values::get_attr(x, symbols().dimnames) {
        dimnames if dimnames.kind() == SexpType::List && length(dimnames) == 2 => {
            dimnames.element(1)
        }
        _ => nil(),
    };
    let dimnames = alloc(SexpType::List, 2)?;
    values::set_elt(dimnames, 0, call.arg(4));
    values::set_elt(dimnames, 1, column_names);
    values::set_attr(result, symbols().dimnames, dimnames)?;
    Ok(result)
}

/// `rowsum_df(x, group, ugroup, na.rm, snames)`: as `rowsum_matrix`, for a data frame of
/// numeric columns, giving a data frame with row names `snames`.
unsafe fn do_rowsum_data_frame(call: &Call<'_>) -> R<Sexp> {
    let frame = call.arg(0);
    let groups = group_positions(call)?;
    let result = alloc(SexpType::List, length(frame))?;
    for index in 0..length(frame) {
        let column = values::materialize(frame.element(index))?;
        if !matches!(
            column.kind(),
            SexpType::Logical | SexpType::Integer | SexpType::Real
        ) || values::inherits(column, "factor")
        {
            return Err(call.error("non-numeric data frame in rowsum"));
        }
        let kind = if column.kind() == SexpType::Real {
            SexpType::Real
        } else {
            SexpType::Integer
        };
        let column = coerce_vector(column, kind)?;
        let sums = alloc(kind, groups.count)?;
        values::set_elt(result, index, sums);
        group_sums(
            call,
            column,
            0..length(column),
            sums,
            0..groups.count,
            &groups,
        )?;
    }
    values::set_attr(
        result,
        symbols().names,
        values::get_attr(frame, symbols().names),
    )?;
    values::set_attr(result, symbols().row_names, call.arg(4))?;
    values::set_attr(
        result,
        symbols().class,
        values::scalar_string("data.frame")?,
    )?;
    Ok(result)
}

struct Groups {
    /// The 0-based group of each row.
    positions: Vec<usize>,
    count: usize,
}

unsafe fn group_positions(call: &Call<'_>) -> R<Groups> {
    let group = call.arg(1);
    let unique = call.arg(2);
    let string_code = SexpType::String.code();
    let kind = if group.kind().code() >= string_code || unique.kind().code() >= string_code {
        SexpType::String
    } else if group.kind().code() >= unique.kind().code() {
        group.kind()
    } else {
        unique.kind()
    };
    let group = values::materialize(coerce_vector(group, kind)?)?;
    let unique = values::materialize(coerce_vector(unique, kind)?)?;
    let missing = || call.error("'ugroup' does not contain every group");
    if let (Some(table_keys), Some(group_keys)) = (plain_keys(unique), plain_keys(group))
        && plain_kinds_match(table_keys, group_keys)
    {
        let mut matched = vec![0i32; length(group)];
        if match_positions(table_keys, group_keys, &mut matched) {
            let positions = matched
                .into_iter()
                .map(|position| position.checked_sub(1).filter(|&index| index >= 0))
                .map(|index| index.map(|index| index as usize).ok_or_else(missing))
                .collect::<R<Vec<usize>>>()?;
            return Ok(Groups {
                positions,
                count: length(unique),
            });
        }
    }
    let mut table: FxHashMap<Key<'_>, usize> = FxHashMap::default();
    for (index, key) in element_keys(unique).into_iter().enumerate() {
        table.entry(key).or_insert(index);
    }
    let positions = element_keys(group)
        .into_iter()
        .map(|key| table.get(&key).copied().ok_or_else(missing))
        .collect::<R<Vec<usize>>>()?;
    Ok(Groups {
        positions,
        count: length(unique),
    })
}

/// Adds `x[input]` into `result[output]` by group. An integer sum that overflows is `NA`, without
/// a warning, as in GNU R.
unsafe fn group_sums(
    call: &Call<'_>,
    x: Sexp,
    input: std::ops::Range<usize>,
    result: Sexp,
    output: std::ops::Range<usize>,
    groups: &Groups,
) -> R<()> {
    let na_rm = values::as_logical_scalar(call.arg(3)) == 1;
    if x.kind() == SexpType::Real {
        let values = &values::reals(x)[input];
        let sums = &mut values::reals_mut(result)[output];
        sums.fill(0.0);
        for (&value, &group) in values.iter().zip(&groups.positions) {
            if !(na_rm && value.is_nan()) {
                sums[group] += value;
            }
        }
        return Ok(());
    }
    let values = &values::ints(x)[input];
    let mut sums = vec![Some(0i64); groups.count];
    for (&value, &group) in values.iter().zip(&groups.positions) {
        if value == NA_INTEGER {
            if !na_rm {
                sums[group] = None;
            }
            continue;
        }
        sums[group] = sums[group].map(|sum| sum + i64::from(value));
    }
    for (slot, sum) in values::ints_mut(result)[output].iter_mut().zip(sums) {
        *slot = match sum {
            Some(sum) if sum > i64::from(i32::MIN) && sum <= i64::from(i32::MAX) => sum as i32,
            _ => NA_INTEGER,
        };
    }
    Ok(())
}

/// `split(x, f)` for a factor `f`: one piece per level, `f` recycled along `x`, and NA codes
/// dropped. Each piece keeps the names of its elements.
unsafe fn do_split(call: &Call<'_>) -> R<Sexp> {
    let x = values::materialize(call.arg(0))?;
    let factor = call.arg(1);
    if !x.kind().is_vector() {
        return Err(error("first argument must be a vector"));
    }
    if !values::inherits(factor, "factor") {
        return Err(error("second argument must be a factor"));
    }
    let levels = get_attr(factor, symbols().levels);
    let level_count = length(levels);
    let codes = values::materialize(coerce_vector(factor, SexpType::Integer)?)?;
    let codes = values::ints(codes);
    let observations = length(x);
    if codes.is_empty() && observations > 0 {
        return Err(error("group length is 0 but data length > 0"));
    }
    if !codes.is_empty() && !observations.is_multiple_of(codes.len()) {
        warning("data length is not a multiple of split variable")?;
    }
    let mut counts: SmallVec<[usize; 16]> = smallvec![0; level_count];
    for &code in codes.iter().cycle().take(observations) {
        if code == NA_INTEGER {
            continue;
        }
        match counts.get_mut(level_of(code)) {
            Some(count) => *count += 1,
            None => return Err(error("factor has bad level")),
        }
    }
    let names = values::names(x);
    let result = alloc(SexpType::List, level_count)?;
    let mut name_pieces = Vec::new();
    for (level, &count) in counts.iter().enumerate() {
        let piece = alloc(x.kind(), count)?;
        values::set_elt(result, level, piece);
        if names != nil() {
            let piece_names = alloc(SexpType::String, count)?;
            values::set_attr(piece, symbols().names, piece_names)?;
            name_pieces.push(piece_names);
        }
    }
    let pieces: Vec<Sexp> = (0..level_count)
        .map(|level| result.element(level))
        .collect();
    let heap = crate::heap();
    match x.kind() {
        SexpType::Logical | SexpType::Integer => {
            let mut targets: Vec<&mut [i32]> =
                pieces.iter().map(|&p| values::ints_mut(p)).collect();
            scatter(values::ints(x), codes, &mut targets);
        }
        SexpType::Real => {
            let mut targets: Vec<&mut [f64]> =
                pieces.iter().map(|&p| values::reals_mut(p)).collect();
            scatter(values::reals(x), codes, &mut targets);
        }
        SexpType::Complex => {
            let mut targets: Vec<&mut [_]> =
                pieces.iter().map(|&p| values::complexes_mut(p)).collect();
            scatter(values::complexes(x), codes, &mut targets);
        }
        SexpType::Raw => {
            let mut targets: Vec<&mut [u8]> = pieces.iter().map(|&p| values::raws_mut(p)).collect();
            scatter(values::raws(x), codes, &mut targets);
        }
        SexpType::String | SexpType::List | SexpType::Expression => {
            for_each_member(codes, observations, level_count, |level, slot, index| {
                heap.set_element(pieces[level], slot, x.element(index));
            });
        }
        _ => {
            let mut failure = None;
            for_each_member(codes, observations, level_count, |level, slot, index| {
                if failure.is_none()
                    && let Err(error) = crate::subset::copy_element(x, index, pieces[level], slot)
                {
                    failure = Some(error);
                }
            });
            if let Some(error) = failure {
                return Err(error);
            }
        }
    }
    if names != nil() {
        let names = values::materialize(names)?;
        for_each_member(codes, observations, level_count, |level, slot, index| {
            heap.set_element(name_pieces[level], slot, names.element(index));
        });
    }
    if levels.kind() == SexpType::String {
        values::set_attr(result, symbols().names, levels)?;
    }
    Ok(result)
}

/// Calls `visit(level, slot, index)` for each observation `index` whose recycled factor code is
/// not `NA`, with `slot` its position within its level's piece. The codes are already checked
/// to lie in `1..=level_count`.
fn for_each_member(
    codes: &[i32],
    observations: usize,
    level_count: usize,
    mut visit: impl FnMut(usize, usize, usize),
) {
    let mut cursors: SmallVec<[usize; 16]> = smallvec![0; level_count];
    for (index, &code) in codes.iter().cycle().take(observations).enumerate() {
        let level = level_of(code);
        if let Some(cursor) = cursors.get_mut(level) {
            visit(level, *cursor, index);
            *cursor += 1;
        }
    }
}

/// Moves each element of `source` into the piece of its level, for elements stored inline.
/// Each target is exactly as long as the count of its level's codes over `source`, recycled.
fn scatter<T: Copy>(source: &[T], codes: &[i32], targets: &mut [&mut [T]]) {
    let mut cursors: SmallVec<[std::slice::IterMut<'_, T>; 16]> =
        targets.iter_mut().map(|target| target.iter_mut()).collect();
    for chunk in source.chunks(codes.len().max(1)) {
        for (&value, &code) in chunk.iter().zip(codes) {
            if let Some(slot) = cursors.get_mut(level_of(code)).and_then(Iterator::next) {
                *slot = value;
            }
        }
    }
}

/// The 0-based level of a factor code, with `NA` mapped past every level.
fn level_of(code: i32) -> usize {
    (code as u32).wrapping_sub(1) as usize
}

unsafe fn do_identical(call: &Call<'_>) -> R<Sexp> {
    let mut settings = [false; 7];
    let names = [
        "num.eq",
        "single.NA",
        "attrib.as.set",
        "ignore.bytecode",
        "ignore.environment",
        "ignore.srcref",
        "extptr.as.ref",
    ];
    for (position, name) in names.iter().enumerate() {
        settings[position] = match as_logical_scalar(call.arg(position + 2)) {
            NA_LOGICAL => return Err(error(format!("invalid '{name}' value"))),
            value => value != 0,
        };
    }
    let flags = IdenticalFlags {
        num_eq: settings[0],
        single_na: settings[1],
        attributes_as_set: settings[2],
        ignore_bytecode: settings[3],
        ignore_environment: settings[4],
        ignore_srcref: settings[5],
        external_pointer_as_reference: settings[6],
    };
    values::scalar_bool(identical(call.arg(0), call.arg(1), &flags))
}

/// A single `TRUE` or `FALSE`, as GNU R's `asLogical` reads it; anything else is `message`.
unsafe fn flag(value: Sexp, message: &str) -> R<bool> {
    match as_logical_scalar(value) {
        NA_LOGICAL => Err(error(message)),
        value => Ok(value != 0),
    }
}

unsafe fn check_sortable(x: Sexp) -> R<()> {
    if !x.kind().is_atomic() {
        return Err(error("only atomic vectors can be sorted"));
    }
    if x.kind() == SexpType::Raw {
        return Err(error("raw vectors cannot be sorted"));
    }
    Ok(())
}

pub(crate) unsafe fn unimplemented_type(object: Sexp, routine: &str) -> crate::unwind::Unwind {
    error(format!(
        "unimplemented type '{}' in '{routine}'\n",
        object.kind().name()
    ))
}

unsafe fn one_based(order: &[usize]) -> R<Sexp> {
    let order: Vec<i32> = order.iter().map(|&index| index as i32 + 1).collect();
    values::integer_vector(&order)
}

/// One sort key's elements, read once so that comparisons do not go back to the heap. Nothing
/// allocates while a column is alive, so the borrowed slices stay valid.
enum Column<'a> {
    /// Logical and integer vectors, and factor codes.
    Integer(&'a [i32]),
    Real(&'a [f64]),
    Complex(&'a [Rcomplex]),
    Raw(&'a [u8]),
    String(Vec<Option<Cow<'a, str>>>),
    /// Strings as the rank of each element's text among the vector's distinct strings (from 1),
    /// with 0 for `NA`, so a string key sorts like an integer one. The ranks follow the
    /// collation for `order`, and bytes, as in the C locale, for the radix sort, which is GNU
    /// R's rule whatever the session's collation; texts that compare equal rank equal.
    Ranks(Vec<u32>),
    /// Strings the radix sort groups without sorting (`sortstr = FALSE`): each element is the
    /// rank of its first appearance, NA included, and the groups keep that order whatever
    /// `decreasing` and `na.last` say. Alongside, each code's rank by bytes (from 1, with 0 for
    /// `NA`), since a run strictly in the opposite byte order reverses instead.
    Grouped(Vec<i32>, Vec<u32>),
}

unsafe fn column<'a>(vector: Sexp) -> Option<Column<'a>> {
    match vector.kind() {
        SexpType::Logical | SexpType::Integer => Some(Column::Integer(values::ints(vector))),
        SexpType::Real => Some(Column::Real(values::reals(vector))),
        SexpType::Complex => Some(Column::Complex(values::complexes(vector))),
        SexpType::Raw => Some(Column::Raw(values::raws(vector))),
        SexpType::String => Some(Column::String(
            (0..length(vector))
                .map(|index| string_at(vector, index))
                .collect(),
        )),
        _ => None,
    }
}

/// A key as the radix sort reads it: no complex keys, and with `sortstr = FALSE` strings in
/// order of first appearance instead of collation order.
unsafe fn radix_column<'a>(vector: Sexp, sort_strings: bool) -> Option<Column<'a>> {
    match vector.kind() {
        // GNU R's radix sort reads neither, and reports one only where it is needed.
        SexpType::Complex | SexpType::Raw => None,
        SexpType::String if !sort_strings => {
            let mut ranks: FxHashMap<Option<Cow<'_, str>>, i32> = FxHashMap::default();
            let codes = (0..length(vector))
                .map(|index| {
                    let next = ranks.len() as i32;
                    *ranks.entry(string_at(vector, index)).or_insert(next)
                })
                .collect();
            // `None` sorts first, so it takes rank 0 and the texts follow from 1.
            let mut distinct: Vec<_> = ranks.into_iter().collect();
            distinct.sort_unstable();
            let shift = u32::from(distinct.first().is_some_and(|(text, _)| text.is_some()));
            let mut byte_ranks = vec![0; distinct.len()];
            for (rank, (_, code)) in (0..).zip(&distinct) {
                byte_ranks[*code as usize] = rank + shift;
            }
            Some(Column::Grouped(codes, byte_ranks))
        }
        SexpType::String => Some(Column::Ranks(string_ranks(vector, TextOrder::Bytes))),
        _ => column(vector),
    }
}

/// Each element's rank in [`Column::Ranks`], with texts ordered as `order` says. R interns its
/// strings, so equal strings are one CHARSXP, and only the distinct CHARSXPs are sorted: each is numbered the first time it is
/// met, with the number negated in its truelength as data.table's C marks strings, which costs
/// one load per row and no table. The saved truelengths are put back before anything else can
/// run.
unsafe fn string_ranks(vector: Sexp, order: TextOrder) -> Vec<u32> {
    let bytewise = matches!(order, TextOrder::Bytes) || c_collation();
    let collator = (!bytewise).then(collator);
    let compare = |left: &str, right: &str| match &collator {
        Some(collator) => collator.compare(left, right),
        None => left.cmp(right),
    };
    let na = na_string();
    let rows = length(vector);
    let mut ranks: Vec<u32> = Vec::with_capacity(rows);
    let mut saved: Vec<(Sexp, i32)> = Vec::new();
    let mut stale = false;
    for index in 0..rows {
        let charsxp = vector.element(index);
        if charsxp == na {
            ranks.push(0);
            continue;
        }
        let mark = charsxp.char_truelength();
        if mark < 0 {
            // A mark this loop did not set (one left behind by C code that stopped midway)
            // would alias two strings, so it is checked against its owner.
            let code = mark.unsigned_abs();
            let ours = (code as usize)
                .checked_sub(1)
                .and_then(|slot| saved.get(slot))
                .is_some_and(|(owner, _)| *owner == charsxp);
            if !ours {
                stale = true;
                break;
            }
            ranks.push(code);
            continue;
        }
        saved.push((charsxp, mark));
        let code = saved.len() as u32;
        charsxp.set_char_truelength(-(code as i32));
        ranks.push(code);
    }
    for (charsxp, mark) in &saved {
        charsxp.set_char_truelength(*mark);
    }
    if stale {
        return string_ranks_by_text(vector, order, &compare);
    }
    let rank_of_code = match order {
        // GNU R's radix sort compares `CHAR()` bytes untranslated, so a latin1 string and its
        // UTF-8 twin rank apart.
        TextOrder::Bytes => {
            let texts: Vec<&[u8]> = saved
                .iter()
                .map(|&(charsxp, _)| {
                    let bytes = charsxp.bytes();
                    memchr::memchr(0, bytes).map_or(bytes, |end| &bytes[..end])
                })
                .collect();
            byte_ranks(&texts)
        }
        // Strings intern by encoding, so one text can be several CHARSXPs, which share a rank.
        TextOrder::Collation => {
            let texts: Vec<Cow<'_, str>> = saved
                .iter()
                .map(|&(charsxp, _)| char_str(charsxp))
                .collect();
            let keys = match &collator {
                Some(collator) => CollationKeys::ascii(&texts).or_else(|| collator.keys(&texts)),
                None => None,
            };
            match keys {
                Some(keys) => byte_ranks(&keys.all()),
                None if bytewise => {
                    let texts: Vec<&[u8]> = texts.iter().map(|text| text.as_bytes()).collect();
                    byte_ranks(&texts)
                }
                None => collation_ranks(&texts, &compare),
            }
        }
    };
    for slot in ranks.iter_mut().filter(|slot| **slot != 0) {
        *slot = rank_of_code[*slot as usize - 1];
    }
    ranks
}

/// How [`string_ranks`] orders texts: by their stored bytes, as the radix sort does whatever
/// the locale, or by the session's collation.
#[derive(Clone, Copy)]
enum TextOrder {
    Bytes,
    Collation,
}

/// The rank of each text (from 1) in collation order, through a comparison sort.
fn collation_ranks(texts: &[Cow<'_, str>], compare: &dyn Fn(&str, &str) -> Ordering) -> Vec<u32> {
    let mut by_text: Vec<u32> = (0..texts.len() as u32).collect();
    by_text
        .sort_unstable_by(|&left, &right| compare(&texts[left as usize], &texts[right as usize]));
    let mut rank_of_code = vec![0u32; texts.len()];
    let mut rank = 0;
    for (position, &code) in by_text.iter().enumerate() {
        if position == 0
            || compare(
                &texts[by_text[position - 1] as usize],
                &texts[code as usize],
            )
            .is_ne()
        {
            rank += 1;
        }
        rank_of_code[code as usize] = rank;
    }
    rank_of_code
}

/// The rank of each text (from 1) in byte order. Eight bytes of each text, zero-padded, form a
/// big-endian key that orders like the bytes themselves, because no R string holds a NUL (and
/// no [`CollationKeys`] key is another followed by zeros). The keys start past the prefix
/// every text shares, are sorted as integers, and a run of equal keys is sorted again on the
/// next eight bytes, so a text's bytes are read once per eight
/// rather than once per comparison.
fn byte_ranks(texts: &[&[u8]]) -> Vec<u32> {
    let mut rank_of_code = vec![0u32; texts.len()];
    let Some(&first) = texts.first() else {
        return rank_of_code;
    };
    let mut entries: Vec<(u64, u32)> = Vec::with_capacity(texts.len());
    let mut common = first.len();
    for (code, &text) in texts.iter().enumerate() {
        common = common.min(
            first[..common.min(text.len())]
                .iter()
                .zip(text)
                .take_while(|(left, right)| left == right)
                .count(),
        );
        entries.push((0, code as u32));
    }
    let mut scratch = Vec::new();
    let mut rank = 0;
    // Runs still to rank, in reverse order, each with the offset its keys start at; `None`
    // marks a run of equal texts.
    let mut pending: Vec<(usize, usize, Option<usize>)> = vec![(0, entries.len(), Some(common))];
    while let Some((start, end, offset)) = pending.pop() {
        let run = &mut entries[start..end];
        let Some(offset) = offset else {
            rank += 1;
            for &(_, code) in run.iter() {
                rank_of_code[code as usize] = rank;
            }
            continue;
        };
        for entry in run.iter_mut() {
            entry.0 = byte_key(texts[entry.1 as usize], offset);
        }
        sort_by_key_bytes(run, &mut scratch);
        let mut runs = Vec::new();
        let mut at = 0;
        while at < run.len() {
            let key = run[at].0;
            let length = run[at..].iter().take_while(|entry| entry.0 == key).count();
            let longer = length > 1
                && run[at..at + length]
                    .iter()
                    .any(|&(_, code)| texts[code as usize].len() > offset + KEY_BYTES);
            runs.push((
                start + at,
                start + at + length,
                longer.then_some(offset + KEY_BYTES),
            ));
            at += length;
        }
        pending.extend(runs.into_iter().rev());
    }
    rank_of_code
}

const KEY_BYTES: usize = 8;

/// The bytes of `text` from `offset`, eight of them zero-padded, as a big-endian integer.
fn byte_key(text: &[u8], offset: usize) -> u64 {
    let mut bytes = [0u8; KEY_BYTES];
    if let Some(rest) = text.get(offset..) {
        let length = rest.len().min(KEY_BYTES);
        bytes[..length].copy_from_slice(&rest[..length]);
    }
    u64::from_be_bytes(bytes)
}

/// Sorts `entries` by key: a short run by comparison, a long one by an LSD radix sort that
/// skips each byte every key shares.
fn sort_by_key_bytes(entries: &mut [(u64, u32)], scratch: &mut Vec<(u64, u32)>) {
    if entries.len() < 256 {
        entries.sort_unstable_by_key(|entry| entry.0);
        return;
    }
    let mut counts = [[0usize; 256]; KEY_BYTES];
    for &(key, _) in entries.iter() {
        for (byte, histogram) in counts.iter_mut().enumerate() {
            histogram[(key >> (8 * byte)) as usize & 0xff] += 1;
        }
    }
    scratch.clear();
    scratch.resize(entries.len(), (0, 0));
    let mut in_scratch = false;
    for (byte, histogram) in counts.iter_mut().enumerate() {
        if histogram.contains(&entries.len()) {
            continue;
        }
        let mut offset = 0;
        for count in histogram.iter_mut() {
            let size = *count;
            *count = offset;
            offset += size;
        }
        let (source, target) = if in_scratch {
            (&scratch[..], &mut *entries)
        } else {
            (&*entries, &mut scratch[..])
        };
        for &entry in source {
            let bucket = (entry.0 >> (8 * byte)) as usize & 0xff;
            target[histogram[bucket]] = entry;
            histogram[bucket] += 1;
        }
        in_scratch = !in_scratch;
    }
    if in_scratch {
        entries.copy_from_slice(scratch);
    }
}

/// [`string_ranks`] through a table of the texts, for a vector whose strings carry truelength
/// marks the CHARSXP numbering cannot use.
unsafe fn string_ranks_by_text(
    vector: Sexp,
    order: TextOrder,
    compare: &dyn Fn(&str, &str) -> Ordering,
) -> Vec<u32> {
    if let TextOrder::Bytes = order {
        let na = na_string();
        let bytes_at = |index: usize| {
            let charsxp = vector.element(index);
            (charsxp != na).then(|| {
                let bytes = charsxp.bytes();
                memchr::memchr(0, bytes).map_or(bytes, |end| &bytes[..end])
            })
        };
        let mut sorted: Vec<&[u8]> = (0..length(vector)).filter_map(bytes_at).collect();
        sorted.sort_unstable();
        sorted.dedup();
        let rank_of: FxHashMap<&[u8], u32> = sorted
            .iter()
            .zip(1..)
            .map(|(&text, rank)| (text, rank))
            .collect();
        return (0..length(vector))
            .map(|index| bytes_at(index).map_or(0, |text| rank_of[text]))
            .collect();
    }
    let mut distinct: FxHashMap<Cow<'_, str>, u32> = FxHashMap::default();
    for index in 0..length(vector) {
        if let Some(text) = string_at(vector, index) {
            distinct.entry(text).or_insert(0);
        }
    }
    let mut sorted: Vec<Cow<'_, str>> = distinct.keys().cloned().collect();
    sorted.sort_unstable_by(|left, right| compare(left, right));
    let mut rank = 0;
    for position in 0..sorted.len() {
        if position == 0 || compare(&sorted[position - 1], &sorted[position]).is_ne() {
            rank += 1;
        }
        distinct.insert(sorted[position].clone(), rank);
    }
    (0..length(vector))
        .map(|index| string_at(vector, index).map_or(0, |text| distinct[&text]))
        .collect()
}

impl Column<'_> {
    fn is_na(&self, index: usize) -> bool {
        match self {
            Column::Integer(items) => items[index] == NA_INTEGER,
            Column::Real(items) => items[index].is_nan(),
            Column::Complex(items) => items[index].r.is_nan() || items[index].i.is_nan(),
            Column::String(items) => items[index].is_none(),
            Column::Ranks(ranks) => ranks[index] == 0,
            Column::Grouped(..) | Column::Raw(_) => false,
        }
    }

    /// Compares two elements, neither of them NA.
    fn compare_values(&self, left: usize, right: usize) -> Ordering {
        match self {
            Column::Integer(items) => items[left].cmp(&items[right]),
            Column::Grouped(items, _) => items[left].cmp(&items[right]),
            Column::Raw(items) => items[left].cmp(&items[right]),
            Column::Real(items) => items[left]
                .partial_cmp(&items[right])
                .unwrap_or(Ordering::Equal),
            Column::Complex(items) => {
                let (left, right) = (items[left], items[right]);
                left.r
                    .partial_cmp(&right.r)
                    .unwrap_or(Ordering::Equal)
                    .then(left.i.partial_cmp(&right.i).unwrap_or(Ordering::Equal))
            }
            Column::String(items) => match (&items[left], &items[right]) {
                (Some(left), Some(right)) => collate(left, right),
                _ => Ordering::Equal,
            },
            Column::Ranks(items) => items[left].cmp(&items[right]),
        }
    }

    /// Compares two elements in increasing order, with NAs equal to each other and placed
    /// after (or before) every other value.
    fn compare(&self, left: usize, right: usize, na_last: bool) -> Ordering {
        self.compare_directed(left, right, na_last, false)
    }

    /// As `compare`, with the non-NA values in decreasing order when `decreasing` is set. NA
    /// placement does not depend on the direction.
    fn compare_directed(
        &self,
        left: usize,
        right: usize,
        na_last: bool,
        decreasing: bool,
    ) -> Ordering {
        match (self.is_na(left), self.is_na(right)) {
            (true, true) => Ordering::Equal,
            (true, false) => {
                if na_last {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, true) => {
                if na_last {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, false) => {
                let ordering = self.compare_values(left, right);
                let decreasing = decreasing && !matches!(self, Column::Grouped(..));
                if decreasing {
                    ordering.reverse()
                } else {
                    ordering
                }
            }
        }
    }
}

/// One key's radix sort keys. Every key but a double fits 32 bits, and since these sorts are
/// bound by memory bandwidth, the narrower keys halve what each pass reads.
enum SortKeys {
    Narrow(Keys<u32>),
    Wide(Keys<u64>),
    Appearance(Appearance),
}

/// Strings ordered by first appearance, as `radixsort(sortstr = FALSE)` orders them. GNU R
/// numbers the strings by their first appearance within each run of rows that tie on the earlier
/// keys, not in the whole column, so before a run sorts by this key its rows' codes are
/// renumbered in that order. Each row lies in one run, so rewriting its code in place is safe.
/// A run whose strings are strictly in the opposite byte order reverses instead, since GNU R
/// checks every run for order by bytes before it groups one.
struct Appearance {
    keys: Keys<u32>,
    /// For each code of the whole column, the run that last renumbered it and its code there.
    renumbered: Vec<(u32, u32)>,
    /// For each code of the whole column, its place in the byte order the check reads.
    byte_order: Vec<u32>,
    /// `na.last = NA`, under which NAs sort first and have byte order 0.
    nas_unsorted: bool,
    run: u32,
}

/// A key's values, with the least and greatest found in the pass that builds them, which spares
/// the sort of every row a pass of its own to find its range.
struct Keys<K> {
    values: Vec<K>,
    low: u64,
    high: u64,
}

/// The keys `key` makes of `items`. Each block of keys is scanned for its range while it is still
/// in cache, as a loop of its own, since folding the range into the loop that makes the keys
/// keeps either from compiling to vector instructions.
fn keys<T: Copy, K: RadixKey>(items: &[T], key: impl Fn(T) -> K) -> Keys<K> {
    const BLOCK: usize = 4096;
    let mut values = Vec::with_capacity(items.len());
    let (mut low, mut high) = (K::narrow(u64::MAX), K::narrow(0));
    for block in items.chunks(BLOCK) {
        let start = values.len();
        values.extend(block.iter().map(|&item| key(item)));
        for &value in &values[start..] {
            low = low.min(value);
            high = high.max(value);
        }
    }
    Keys {
        values,
        low: low.wide(),
        high: high.wide(),
    }
}

impl SortKeys {
    fn compare(&self, left: usize, right: usize) -> Ordering {
        match self {
            SortKeys::Narrow(keys) | SortKeys::Appearance(Appearance { keys, .. }) => {
                keys.values[left].cmp(&keys.values[right])
            }
            SortKeys::Wide(keys) => keys.values[left].cmp(&keys.values[right]),
        }
    }

    fn equal(&self, left: usize, right: usize) -> bool {
        self.compare(left, right).is_eq()
    }
}

/// Each element of one radix sort key as an unsigned integer whose natural order is the key's
/// order: decreasing when `decreasing` is set (except for first-appearance codes, which keep
/// their order), with every NA equal to every other and placed last or first. `None` for a key
/// the radix sort does not read.
fn sort_keys(column: &Column<'_>, decreasing: bool, na_last: bool) -> Option<SortKeys> {
    // Non-NA keys lie strictly between 0 and `top`, so NAs take one end of the range.
    let place = |key: Option<u64>, top: u64| match key {
        Some(key) if decreasing => top - key,
        Some(key) => key,
        None if na_last => top,
        None => 0,
    };
    match column {
        Column::Integer(items) => {
            // Flipping the sign bit orders an `i32` as a `u32` and maps `NA`, the smallest
            // `i32`, to 0, so the other keys lie in 1..=u32::MAX and one end is shifted to
            // make room for `NA` where it sorts last.
            let shifted = |value: i32| (value as u32) ^ (1 << 31);
            Some(SortKeys::Narrow(match (decreasing, na_last) {
                (false, false) => keys(items, shifted),
                (false, true) => keys(items, |value| shifted(value).wrapping_sub(1)),
                (true, false) => keys(items, |value| match shifted(value) {
                    0 => 0,
                    key => (u32::MAX - key) + 1,
                }),
                (true, true) => keys(items, |value| match shifted(value) {
                    0 => u32::MAX,
                    key => u32::MAX - key,
                }),
            }))
        }
        Column::Real(items) => Some(SortKeys::Wide(keys(items, |value: f64| {
            // The IEEE bits ordered as unsigned integers, with -0 read as 0. NaN bit patterns
            // map past both infinities, so 0 and `u64::MAX` stay free.
            let key = (!value.is_nan()).then(|| {
                let bits = if value == 0.0 { 0 } else { value.to_bits() };
                if bits >> 63 == 1 {
                    !bits
                } else {
                    bits | 1 << 63
                }
            });
            place(key, u64::MAX)
        }))),
        Column::Grouped(items, byte_ranks) => {
            // GNU R's `StrCmp2`: bytes in the direction asked, and NA at the end `na.last` says.
            let top = byte_ranks.len() as u32 + 1;
            let byte_order = byte_ranks
                .iter()
                .map(|&rank| match rank {
                    0 if na_last => u32::MAX,
                    0 => 0,
                    rank if decreasing => top - rank,
                    rank => rank,
                })
                .collect();
            Some(SortKeys::Appearance(Appearance {
                keys: keys(items, |code| code as u32),
                renumbered: vec![(0, 0); byte_ranks.len()],
                byte_order,
                nas_unsorted: false,
                run: 0,
            }))
        }
        Column::Ranks(ranks) => {
            // The ranks count the rows at most, which are fewer than `u32::MAX`.
            let top = u64::from(ranks.iter().copied().max().unwrap_or(0)) + 1;
            Some(SortKeys::Narrow(keys(ranks, |rank| {
                place((rank != 0).then_some(u64::from(rank)), top) as u32
            })))
        }
        Column::Complex(_) | Column::String(_) | Column::Raw(_) => None,
    }
}

/// Writes into `order` the stable 0-based order of its rows by `keys`, the first key most
/// significant. The first key sorts every row, and each run of rows that tie on it then sorts
/// by the remaining keys, so a later key is only ever sorted within a run, whose keys stay in
/// cache. Within a run the rows are still in their original order, which is what lets a run
/// sort `(key, row)` pairs with an unstable algorithm.
fn radix_order(keys: &mut [SortKeys], order: &mut [u32]) {
    sort_runs(keys, order, true, 0, None);
}

/// [`radix_order`], returning the 1-based end of each group of rows that tie on every key, as
/// `radixsort(retgrp = TRUE)` reports them. The groups come from the runs the sort finds anyway,
/// so no pass compares neighbouring rows' keys again.
fn radix_groups(keys: &mut [SortKeys], order: &mut [u32]) -> Vec<i32> {
    let mut ties = Vec::new();
    sort_runs(keys, order, true, 0, Some(&mut ties));
    let mut ends = Vec::with_capacity(
        order.len()
            - ties
                .iter()
                .map(|(start, end)| end - start - 1)
                .sum::<usize>(),
    );
    let mut position = 0;
    for (start, end) in ties {
        ends.extend((position + 1..=start).map(|end| end as i32));
        ends.push(end as i32);
        position = end;
    }
    ends.extend((position + 1..=order.len()).map(|end| end as i32));
    ends
}

/// Sorts `order` by `keys`, most significant first, where rows that tie on every key are in
/// increasing order already. With `identity` the rows are `0..order.len()` and `order` holds
/// nothing yet. With `ties`, it appends in increasing order the ranges, offset by `offset`, of
/// two or more rows that tie on every key.
fn sort_runs(
    keys: &mut [SortKeys],
    order: &mut [u32],
    identity: bool,
    offset: usize,
    mut ties: Option<&mut Vec<(usize, usize)>>,
) {
    // A run this short sorts by comparing rows, which costs less than gathering its keys.
    const SHORT_RUN: usize = 32;
    let Some((key, rest)) = keys.split_first_mut() else {
        if identity {
            fill_identity(order);
        }
        return;
    };
    let want_runs = !rest.is_empty() || ties.is_some();
    let runs = match key {
        SortKeys::Narrow(key) => sort_by_key(key, order, identity, want_runs),
        SortKeys::Wide(key) => sort_by_key(key, order, identity, want_runs),
        SortKeys::Appearance(key) => {
            if identity {
                fill_identity(order);
            }
            // GNU R's `csorted` calls a run with some but not all NAs unsorted when
            // `na.last = NA`, so such a run is never reversed.
            let code = |row: u32| key.keys.values[row as usize] as usize;
            let strictly_reversed = order.len() > 1
                && !(key.nas_unsorted && order.iter().any(|&row| key.byte_order[code(row)] == 0))
                && order
                    .windows(2)
                    .all(|pair| key.byte_order[code(pair[1])] < key.byte_order[code(pair[0])]);
            if strictly_reversed {
                // Strictly ordered rows are all distinct, so no run ties.
                order.reverse();
                Vec::new()
            } else {
                renumber(key, order);
                sort_by_key(&key.keys, order, false, want_runs)
            }
        }
    };
    // Comparing rows reads codes that only a run of their own renumbers.
    let comparable = !rest
        .iter()
        .any(|key| matches!(key, SortKeys::Appearance(_)));
    for (start, end) in runs {
        let run = &mut order[start..end];
        if rest.is_empty() {
            if let Some(ties) = ties.as_deref_mut() {
                ties.push((offset + start, offset + end));
            }
        } else if run.len() > SHORT_RUN || !comparable {
            sort_runs(rest, run, false, offset + start, ties.as_deref_mut());
        } else {
            run.sort_by(|&left, &right| {
                rest.iter()
                    .map(|key| key.compare(left as usize, right as usize))
                    .find(|ordering| ordering.is_ne())
                    .unwrap_or(Ordering::Equal)
            });
            if let Some(ties) = ties.as_deref_mut() {
                let mut group = 0;
                for position in 1..=run.len() {
                    if position == run.len()
                        || rest.iter().any(|key| {
                            !key.equal(run[position - 1] as usize, run[position] as usize)
                        })
                    {
                        if position - group > 1 {
                            ties.push((offset + start + group, offset + start + position));
                        }
                        group = position;
                    }
                }
            }
        }
    }
}

/// Numbers the codes of `rows`, a run in increasing row order, by their first appearance in it.
fn renumber(key: &mut Appearance, rows: &[u32]) {
    key.run += 1;
    let mut next = 0;
    for &row in rows {
        let code = &mut key.keys.values[row as usize];
        let slot = &mut key.renumbered[*code as usize];
        if slot.0 != key.run {
            *slot = (key.run, next);
            next += 1;
        }
        *code = slot.1;
    }
}

fn fill_identity(order: &mut [u32]) {
    for (position, slot) in order.iter_mut().enumerate() {
        *slot = position as u32;
    }
}

/// Sorts `order` by one key, as `sort_runs` describes its rows, and with `want_runs` returns
/// the ranges of rows that tie on the key, longer than one row. Rows already in key order cost
/// one pass, a dense key (such as small-range integers or factor codes) one counting pass, a
/// short slice a comparison sort of `(key, row)` pairs, and anything else a most significant
/// digit radix sort of those pairs. Every pass counts, since these sorts are bound by memory
/// bandwidth.
fn sort_by_key<K: RadixKey>(
    keys: &Keys<K>,
    order: &mut [u32],
    identity: bool,
    want_runs: bool,
) -> Vec<(usize, usize)> {
    let key = &keys.values[..];
    let count = order.len();
    // The keys gather once into row order, so every pass below streams one contiguous slice
    // instead of chasing rows through the key column.
    let rows: Option<Vec<u32>> = (!identity).then(|| order.to_vec());
    let gathered: Option<Vec<K>> = rows
        .as_ref()
        .map(|rows| rows.iter().map(|&row| key[row as usize]).collect());
    let values = gathered.as_deref().unwrap_or(key);
    let mut runs = Vec::new();
    if values.is_sorted() {
        if identity {
            fill_identity(order);
        }
        if want_runs {
            push_runs(&mut runs, 0, count, |position| values[position].wide());
        }
        return runs;
    }
    let (low, high) = if identity {
        (keys.low, keys.high)
    } else {
        values.iter().fold((u64::MAX, 0), |(low, high), value| {
            (low.min(value.wide()), high.max(value.wide()))
        })
    };
    let span = high - low;
    // Past about a million buckets the counting passes miss cache on every row, and the radix
    // sort's sequential scatters win. A short slice counts only while its buckets are few
    // beside its rows, since zeroing them would cost more than the comparison sort.
    let counting_limit = if count > SHORT_SORT {
        (count.max(1 << 16) as u64).min(1 << 20)
    } else {
        4 * count as u64
    };
    if span < counting_limit {
        // Positions fit `u32` like the rows do, and the narrower buckets halve the memory the
        // counting pass touches at random.
        let mut starts = vec![0u32; span as usize + 2];
        for value in values {
            starts[(value.wide() - low) as usize + 1] += 1;
        }
        for bucket in 1..starts.len() {
            starts[bucket] += starts[bucket - 1];
        }
        if want_runs {
            runs.extend(
                starts
                    .windows(2)
                    .filter(|pair| pair[1] - pair[0] > 1)
                    .map(|pair| (pair[0] as usize, pair[1] as usize)),
            );
        }
        match &rows {
            None => {
                for (row, value) in (0..).zip(values) {
                    let start = &mut starts[(value.wide() - low) as usize];
                    order[*start as usize] = row;
                    *start += 1;
                }
            }
            Some(rows) => {
                for (&row, value) in rows.iter().zip(values) {
                    let start = &mut starts[(value.wide() - low) as usize];
                    order[*start as usize] = row;
                    *start += 1;
                }
            }
        }
        return runs;
    }
    let row = |position: usize| rows.as_ref().map_or(position as u32, |rows| rows[position]);
    let mut sink = Sink {
        order,
        runs: want_runs.then_some(&mut runs),
    };
    if count <= SHORT_SORT {
        let mut pairs: Vec<(K, u32)> = (0..count)
            .map(|position| (values[position], row(position)))
            .collect();
        pairs.sort_unstable();
        sink.emit(0, &pairs);
        return runs;
    }
    // The first digit scatters the rows straight into `order`. Each bucket then sorts on its
    // own: one short enough gathers its `(key, row)` pairs into buffers sized to it, and a
    // larger one scatters its rows by the next digit first. Rows alone move until a bucket is
    // short, so the scratch is a `u32` per row at most, not two buffers of pairs.
    let mut shift = u64::BITS - span.leading_zeros();
    let sizes = loop {
        shift = shift.saturating_sub(DIGIT);
        let mut sizes = vec![0usize; BUCKETS];
        for value in values {
            sizes[digit(value.wide() - low, shift)] += 1;
        }
        if shift == 0 || sizes.iter().all(|&size| size < count) {
            break sizes;
        }
    };
    let mut cursors = bucket_starts(&sizes);
    for (position, value) in values.iter().enumerate() {
        let cursor = &mut cursors[digit(value.wide() - low, shift)];
        sink.order[*cursor] = row(position);
        *cursor += 1;
    }
    drop(gathered);
    drop(rows);
    let mut buffers = RowBuffers {
        pairs: Vec::new(),
        scratch: Vec::new(),
        rows: Vec::new(),
    };
    for (start, &size) in bucket_starts(&sizes).into_iter().zip(&sizes) {
        sort_bucket(key, low, &mut sink, start, size, shift, &mut buffers);
    }
    runs
}

/// Above this many rows a bucket scatters its rows by the next digit instead of gathering
/// pairs, which bounds the pair buffers at a few megabytes.
const GATHER: usize = 1 << 16;

/// The scratch of [`sort_bucket`], grown to the largest bucket that needs it.
struct RowBuffers<K> {
    pairs: Vec<(K, u32)>,
    scratch: Vec<(K, u32)>,
    rows: Vec<u32>,
}

/// Sorts the `size` rows at `offset` in the sink's order, whose keys less `low` agree above bit
/// `shift`, and which are in increasing row order where their keys tie.
fn sort_bucket<K: RadixKey>(
    key: &[K],
    low: u64,
    sink: &mut Sink<'_>,
    offset: usize,
    size: usize,
    shift: u32,
    buffers: &mut RowBuffers<K>,
) {
    if size <= 1 {
        return;
    }
    if shift == 0 {
        if let Some(runs) = sink.runs.as_deref_mut() {
            runs.push((offset, offset + size));
        }
        return;
    }
    let range = offset..offset + size;
    if size <= GATHER {
        buffers.pairs.clear();
        buffers.pairs.extend(
            sink.order[range]
                .iter()
                .map(|&row| (K::narrow(key[row as usize].wide() - low), row)),
        );
        buffers.scratch.resize(size, (K::narrow(0), 0));
        let mut pairs = std::mem::take(&mut buffers.pairs);
        let mut scratch = std::mem::take(&mut buffers.scratch);
        msd_sort(&mut pairs, &mut scratch[..size], shift, sink, offset);
        buffers.pairs = pairs;
        buffers.scratch = scratch;
        return;
    }
    let shift = shift.saturating_sub(DIGIT);
    let mut sizes = [0usize; BUCKETS];
    for &row in &sink.order[range.clone()] {
        sizes[digit(key[row as usize].wide() - low, shift)] += 1;
    }
    if !sizes.contains(&size) {
        buffers.rows.clear();
        buffers.rows.extend_from_slice(&sink.order[range]);
        let mut cursors = bucket_starts(&sizes);
        for &row in &buffers.rows {
            let cursor = &mut cursors[digit(key[row as usize].wide() - low, shift)];
            sink.order[offset + *cursor] = row;
            *cursor += 1;
        }
    }
    for (start, &bucket) in bucket_starts(&sizes).iter().zip(&sizes) {
        sort_bucket(key, low, sink, offset + start, bucket, shift, buffers);
    }
}

/// Below this many pairs a comparison sort beats another radix pass.
const SHORT_SORT: usize = 1024;

const DIGIT: u32 = 11;
const BUCKETS: usize = 1 << DIGIT;

/// A radix sort key: a `u32` where the column's keys fit one, which halves the pairs the sort
/// moves, and a `u64` otherwise.
trait RadixKey: Copy + Ord {
    fn wide(self) -> u64;
    /// `value` known to fit.
    fn narrow(value: u64) -> Self;
}

impl RadixKey for u32 {
    fn wide(self) -> u64 {
        u64::from(self)
    }

    fn narrow(value: u64) -> Self {
        value as u32
    }
}

impl RadixKey for u64 {
    fn wide(self) -> u64 {
        self
    }

    fn narrow(value: u64) -> Self {
        value
    }
}

fn digit(key: u64, shift: u32) -> usize {
    ((key >> shift) as usize) & (BUCKETS - 1)
}

fn bucket_starts(sizes: &[usize]) -> Vec<usize> {
    let mut next = 0;
    sizes
        .iter()
        .map(|&size| {
            let start = next;
            next += size;
            start
        })
        .collect()
}

/// Where sorted pairs go: their rows into `order` at the pairs' positions, and with `runs` the
/// ranges of rows whose keys tie.
struct Sink<'a> {
    order: &'a mut [u32],
    runs: Option<&'a mut Vec<(usize, usize)>>,
}

impl Sink<'_> {
    fn emit<K: RadixKey>(&mut self, offset: usize, pairs: &[(K, u32)]) {
        for (slot, &(_, row)) in self.order[offset..].iter_mut().zip(pairs) {
            *slot = row;
        }
        if let Some(runs) = self.runs.as_deref_mut() {
            push_runs(runs, offset, pairs.len(), |position| {
                pairs[position - offset].0.wide()
            });
        }
    }
}

/// Pushes the ranges within `offset..offset + count` whose `value_at` ties, longer than one.
fn push_runs(
    runs: &mut Vec<(usize, usize)>,
    offset: usize,
    count: usize,
    value_at: impl Fn(usize) -> u64,
) {
    let end_of_range = offset + count;
    let mut start = offset;
    while start < end_of_range {
        let value = value_at(start);
        let mut end = start + 1;
        while end < end_of_range && value_at(end) == value {
            end += 1;
        }
        if end - start > 1 {
            runs.push((start, end));
        }
        start = end;
    }
}

/// Sorts `pairs`, whose keys agree above bit `bits`, by a most significant digit radix sort
/// and hands the result to `sink` at `offset`. Each digit scatters the pairs into `scratch`,
/// and each bucket then sorts on its own with the two buffers' roles swapped, so a level costs
/// two passes and no copy, and after the first digit or two a bucket fits in cache. A short
/// bucket finishes with a comparison sort, and since the scatter is stable and the comparison
/// breaks ties by row, rows that tie keep their order.
fn msd_sort<K: RadixKey>(
    pairs: &mut [(K, u32)],
    scratch: &mut [(K, u32)],
    bits: u32,
    sink: &mut Sink<'_>,
    offset: usize,
) {
    if pairs.len() <= SHORT_SORT {
        pairs.sort_unstable();
        sink.emit(offset, pairs);
        return;
    }
    let shift = bits.saturating_sub(DIGIT);
    let mut sizes = [0usize; BUCKETS];
    for &(key, _) in pairs.iter() {
        sizes[digit(key.wide(), shift)] += 1;
    }
    if sizes.contains(&pairs.len()) {
        if shift == 0 {
            sink.emit(offset, pairs);
        } else {
            msd_sort(pairs, scratch, shift, sink, offset);
        }
        return;
    }
    let starts = bucket_starts(&sizes);
    let mut cursors = starts.clone();
    for &pair in pairs.iter() {
        let cursor = &mut cursors[digit(pair.0.wide(), shift)];
        scratch[*cursor] = pair;
        *cursor += 1;
    }
    for (&start, &size) in starts.iter().zip(&sizes) {
        let end = start + size;
        if shift == 0 || size == 1 {
            sink.emit(offset + start, &scratch[start..end]);
        } else if size > 1 {
            msd_sort(
                &mut scratch[start..end],
                &mut pairs[start..end],
                shift,
                sink,
                offset + start,
            );
        }
    }
}

/// Compares two rows key by key. A key of a type that cannot be ordered is recorded in
/// `unsupported` the first time a comparison needs it, which is exactly when GNU R reports it:
/// only rows that tie on every earlier key reach it.
fn compare_rows(
    columns: &[Option<Column<'_>>],
    directions: &[bool],
    left: usize,
    right: usize,
    na_last: bool,
    unsupported: &Cell<Option<usize>>,
) -> Ordering {
    for (position, column) in columns.iter().enumerate() {
        let Some(column) = column else {
            if unsupported.get().is_none() {
                unsupported.set(Some(position));
            }
            return Ordering::Equal;
        };
        let decreasing = directions.get(position).copied().unwrap_or(false);
        let ordering = column.compare_directed(left, right, na_last, decreasing);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

/// Complex order: by real part, then imaginary part. Any comparison with NaN is false.
fn complex_less(left: Rcomplex, right: Rcomplex) -> bool {
    left.r < right.r || (left.r == right.r && left.i < right.i)
}

/// GNU R's `rsort_with_index`: a shell sort with Knuth's gaps (1, 4, 13, 40, ...) that orders
/// `(key, payload)` pairs by key, NaN last. It is not stable, and graphics code that draws in
/// this order (`persp`'s facets) must break ties the way GNU R does.
pub fn rsort_with_index<T: Copy>(items: &mut [(f64, T)]) {
    let greater = |left: f64, right: f64| match (left.is_nan(), right.is_nan()) {
        (true, false) => true,
        (false, false) => left > right,
        _ => false,
    };
    let count = items.len();
    let mut gap = 1;
    while gap <= count / 9 {
        gap = 3 * gap + 1;
    }
    while gap > 0 {
        for index in gap..count {
            let item = items[index];
            let mut slot = index;
            while slot >= gap && greater(items[slot - gap].0, item.0) {
                items[slot] = items[slot - gap];
                slot -= gap;
            }
            items[slot] = item;
        }
        gap /= 3;
    }
}

/// Shell sort with Sedgewick's gaps 4^k + 3 * 2^(k - 1) + 1 (1, 8, 23, 77, 281, ...), moving an
/// element while its predecessor is `greater`.
pub(crate) fn shell_sort<T: Copy>(items: &mut [T], greater: impl Fn(T, T) -> bool) {
    let count = items.len();
    let mut gaps = vec![1usize];
    for exponent in 1..16u32 {
        let gap = 4usize.pow(exponent) + 3 * 2usize.pow(exponent - 1) + 1;
        if gap > count {
            break;
        }
        gaps.push(gap);
    }
    for &gap in gaps.iter().rev() {
        for index in gap..count {
            let value = items[index];
            let mut slot = index;
            while slot >= gap && greater(items[slot - gap], value) {
                items[slot] = items[slot - gap];
                slot -= gap;
            }
            items[slot] = value;
        }
    }
}

/// Places the element of rank `target` (a position in `order`) correctly within
/// `order[low..=high]`, with nothing greater before it and nothing smaller after it. This is
/// Hoare's selection as Wirth presents it.
fn select<T: Copy>(
    order: &mut [T],
    low: isize,
    high: isize,
    target: usize,
    compare: &impl Fn(T, T) -> Ordering,
) {
    let target = target as isize;
    let (mut left, mut right) = (low, high);
    while left < right {
        let pivot = order[target as usize];
        let (mut forward, mut backward) = (left, right);
        while forward <= backward {
            while forward < right && compare(order[forward as usize], pivot) == Ordering::Less {
                forward += 1;
            }
            while backward > left && compare(pivot, order[backward as usize]) == Ordering::Less {
                backward -= 1;
            }
            if forward <= backward {
                order.swap(forward as usize, backward as usize);
                forward += 1;
                backward -= 1;
            }
        }
        if backward < target {
            left = forward;
        }
        if target < forward {
            right = backward;
        }
    }
}

/// Selects every position in `positions` (sorted) within `order[low..=high]`: the position
/// nearest the middle first, then the ones on each side of it within their halves.
fn partial_sort<T: Copy>(
    order: &mut [T],
    low: isize,
    high: isize,
    positions: &[usize],
    compare: &impl Fn(T, T) -> Ordering,
) {
    if positions.is_empty() || high - low < 1 {
        return;
    }
    if let [only] = positions {
        select(order, low, high, *only, compare);
        return;
    }
    let middle = (low + high) / 2;
    let chosen = positions
        .iter()
        .rposition(|&position| position as isize <= middle)
        .unwrap_or(0);
    let pivot = positions[chosen];
    select(order, low, high, pivot, compare);
    partial_sort(
        order,
        low,
        pivot as isize - 1,
        &positions[..chosen],
        compare,
    );
    partial_sort(
        order,
        pivot as isize + 1,
        high,
        &positions[chosen + 1..],
        compare,
    );
}

/// Singleton's quicksort (CACM Algorithm 347, with a varying pivot position and insertion sort
/// for short segments), carrying `index` along. Positions are 1-based, as in the published
/// algorithm.
fn singleton_sort<T: Copy + PartialOrd>(items: &mut [T], index: &mut [i32]) {
    enum Step {
        NewSegment,
        Partition,
        Dispatch,
        Insert,
        Pop,
    }
    let count = items.len() as isize;
    if count < 2 {
        return;
    }
    let get = |items: &[T], position: isize| items[(position - 1) as usize];
    let swap = |items: &mut [T], index: &mut [i32], left: isize, right: isize| {
        items.swap((left - 1) as usize, (right - 1) as usize);
        index.swap((left - 1) as usize, (right - 1) as usize);
    };
    let mut lower = [0isize; 40];
    let mut upper = [0isize; 40];
    let mut ratio = 0.375;
    let mut depth = 1usize;
    let (mut start, mut end) = (1isize, count);
    let mut step = Step::NewSegment;
    loop {
        match step {
            Step::NewSegment => {
                if start == end {
                    step = Step::Pop;
                    continue;
                }
                ratio = if ratio < 0.5898437 {
                    ratio + 0.0390625
                } else {
                    ratio - 0.5859375
                };
                step = Step::Partition;
            }
            Step::Partition => {
                let mut forward = start;
                let middle = start + ((end - start) as f64 * ratio) as isize;
                if get(items, start) > get(items, middle) {
                    swap(items, index, start, middle);
                }
                let mut backward = end;
                if get(items, end) < get(items, middle) {
                    swap(items, index, end, middle);
                    if get(items, start) > get(items, middle) {
                        swap(items, index, start, middle);
                    }
                }
                let pivot = get(items, middle);
                loop {
                    backward -= 1;
                    while backward > start && get(items, backward) > pivot {
                        backward -= 1;
                    }
                    forward += 1;
                    while forward < end && get(items, forward) < pivot {
                        forward += 1;
                    }
                    if forward > backward {
                        break;
                    }
                    swap(items, index, backward, forward);
                }
                if depth >= lower.len() {
                    return;
                }
                if backward - start <= end - forward {
                    lower[depth] = forward;
                    upper[depth] = end;
                    end = backward;
                } else {
                    lower[depth] = start;
                    upper[depth] = backward;
                    start = forward;
                }
                depth += 1;
                step = Step::Dispatch;
            }
            Step::Dispatch => {
                step = if end - start > 10 {
                    Step::Partition
                } else if start == 1 {
                    Step::NewSegment
                } else {
                    start -= 1;
                    Step::Insert
                };
            }
            Step::Insert => {
                start += 1;
                if start >= end {
                    step = Step::Pop;
                    continue;
                }
                let value = get(items, start + 1);
                let value_index = index[start as usize];
                if get(items, start) > value {
                    let mut slot = start;
                    loop {
                        items[slot as usize] = items[(slot - 1) as usize];
                        index[slot as usize] = index[(slot - 1) as usize];
                        slot -= 1;
                        if slot < 1 || value.partial_cmp(&get(items, slot)) != Some(Ordering::Less)
                        {
                            break;
                        }
                    }
                    items[slot as usize] = value;
                    index[slot as usize] = value_index;
                }
            }
            Step::Pop => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
                start = lower[depth];
                end = upper[depth];
                step = Step::Dispatch;
            }
        }
    }
}

/// A hashable stand-in for one atomic element, equal exactly when GNU R's `unique` and `match`
/// consider two elements equal: NA equals NA, NaN equals NaN, NA differs from NaN, and `-0`
/// equals `0`.
#[derive(PartialEq, Eq, Hash)]
enum Key<'a> {
    Integer(i32),
    Real(u64),
    Complex(u64, u64),
    String(Option<Cow<'a, str>>),
    Raw(u8),
}

/// The keys of an atomic vector's elements. Other types give no keys.
unsafe fn element_keys<'a>(vector: Sexp) -> Vec<Key<'a>> {
    match vector.kind() {
        SexpType::Logical | SexpType::Integer => values::ints(vector)
            .iter()
            .map(|&value| Key::Integer(value))
            .collect(),
        SexpType::Real => values::reals(vector)
            .iter()
            .map(|&value| Key::Real(real_key(value)))
            .collect(),
        SexpType::Complex => values::complexes(vector)
            .iter()
            .map(|value| {
                if is_na_real(value.r) || is_na_real(value.i) {
                    Key::Complex(NA_REAL_BITS, NA_REAL_BITS)
                } else {
                    Key::Complex(real_key(value.r), real_key(value.i))
                }
            })
            .collect(),
        SexpType::String => (0..length(vector))
            .map(|index| Key::String(string_at(vector, index)))
            .collect(),
        SexpType::Raw => values::raws(vector)
            .iter()
            .map(|&value| Key::Raw(value))
            .collect(),
        _ => Vec::new(),
    }
}

/// The bits of a double with every NA, every other NaN, and both zeros each made one value.
fn real_key(value: f64) -> u64 {
    if is_na_real(value) {
        NA_REAL_BITS
    } else if value.is_nan() {
        f64::NAN.to_bits()
    } else if value == 0.0 {
        0
    } else {
        value.to_bits()
    }
}

/// The `incomparables` argument as keys of `kind`, or `None` for `NULL` and `FALSE`.
unsafe fn incomparable_keys<'a>(incomparables: Sexp, kind: SexpType) -> R<Option<Vec<Key<'a>>>> {
    if !incomparable_keys_given(incomparables) {
        return Ok(None);
    }
    let converted = values::materialize(coerce_vector(incomparables, kind)?)?;
    Ok(Some(element_keys(converted)))
}

/// Whether `incomparables` names values at all: `NULL` and `FALSE` do not.
unsafe fn incomparable_keys_given(incomparables: Sexp) -> bool {
    !(incomparables == nil()
        || (incomparables.kind() == SexpType::Logical
            && length(incomparables) == 1
            && values::ints(incomparables)[0] == 0))
}

/// Which elements of `x` repeat an earlier one (a later one with `from_last`). Lists compare
/// their elements with `identical()`. With `first_only`, only the first duplicate in visiting
/// order is sure to be flagged, and a hashed scan may stop there.
pub(crate) unsafe fn duplicated_elements(
    x: Sexp,
    incomparables: Sexp,
    from_last: bool,
    first_only: bool,
    what: &str,
) -> R<Bits> {
    if !x.kind().is_vector() {
        return Err(error(format!("{what}() applies only to vectors")));
    }
    let x = values::materialize(x)?;
    let count = length(x);
    let list = matches!(x.kind(), SexpType::List | SexpType::Expression);
    // A list's incomparables are elements compared with `identical()`, as GNU R coerces them to
    // the type of `x`.
    let incomparable_elements = if list && incomparable_keys_given(incomparables) {
        let converted = coerce_vector(incomparables, x.kind())?;
        crate::heap().root(converted);
        Some(converted)
    } else {
        None
    };
    let incomparables = if list {
        None
    } else {
        incomparable_keys(incomparables, x.kind())?
    };
    if incomparables.is_none()
        && let Some(keys) = plain_keys(x)
        && let Some(flags) = duplicate_flags(keys, from_last, first_only)
    {
        return Ok(flags);
    }
    let mut duplicated = Bits::new(count);
    let visit: Box<dyn Iterator<Item = usize>> = if from_last {
        Box::new((0..count).rev())
    } else {
        Box::new(0..count)
    };
    if list {
        let flags = IdenticalFlags::default();
        let hashes: Vec<u64> = (0..count)
            .map(|index| structural_hash(x.element(index)))
            .collect();
        let key_at = |index: usize| ListElement {
            hash: hashes[index],
            element: x.element(index),
        };
        let mut table = PositionTable::new(count);
        for index in visit {
            let key = key_at(index);
            let same = |left: ListElement, right: ListElement| {
                left.hash == right.hash && identical(left.element, right.element, &flags)
            };
            if table.find_or_insert_by(key, index, key_at, same).is_some() {
                let element = key.element;
                let incomparable = incomparable_elements.is_some_and(|elements| {
                    (0..length(elements))
                        .any(|position| identical(elements.element(position), element, &flags))
                });
                if !incomparable {
                    duplicated.insert(index);
                }
            }
        }
        return Ok(duplicated);
    }
    let keys = element_keys(x);
    let mut seen: FxHashSet<&Key<'_>> = FxHashSet::default();
    for index in visit {
        let key = &keys[index];
        if !seen.insert(key)
            && !incomparables
                .as_ref()
                .is_some_and(|keys| keys.contains(key))
        {
            duplicated.insert(index);
        }
    }
    Ok(duplicated)
}

/// A vector's elements as keys that hash without conversion: integers and logicals as they are,
/// doubles through `real_key`, and strings as their interned string objects. Interning makes
/// equal ASCII text one object (`NA` included), but equal text in two encodings is two objects,
/// so a string table checks [`TableKey::exact`] on each distinct key and each miss rather than
/// scanning every element first.
#[derive(Clone, Copy)]
enum PlainKeys<'a> {
    Integer(&'a [i32]),
    Real(&'a [f64]),
    Interned(&'a [Sexp]),
}

/// `None` for other kinds, and for vectors too long for the 32-bit positions of `PositionTable`.
unsafe fn plain_keys<'a>(vector: Sexp) -> Option<PlainKeys<'a>> {
    if length(vector) > i32::MAX as usize {
        return None;
    }
    match vector.kind() {
        SexpType::Logical | SexpType::Integer => Some(PlainKeys::Integer(values::ints(vector))),
        SexpType::Real => Some(PlainKeys::Real(values::reals(vector))),
        SexpType::String => Some(PlainKeys::Interned(vector.slice::<Sexp>())),
        _ => None,
    }
}

/// Whether [`match_positions`] can match `x` against `table`: both sides have the same kind.
fn plain_kinds_match(table: PlainKeys<'_>, x: PlainKeys<'_>) -> bool {
    matches!(
        (table, x),
        (PlainKeys::Integer(_), PlainKeys::Integer(_))
            | (PlainKeys::Real(_), PlainKeys::Real(_))
            | (PlainKeys::Interned(..), PlainKeys::Interned(..))
    )
}

/// Writes to `positions`, for each element of `x`, the 1-based position of the first equal
/// element of `table`, or 0. Both sides have the same kind ([`plain_kinds_match`]). `false` when
/// strings that are not ASCII make pointer equality inexact, and the general path must run.
///
/// # Safety
///
/// `table` and `x` must come from [`plain_keys`] on ordinary (not ALTREP) vectors that stay
/// rooted and unmodified for the call, so that their slices and `CHARSXP` pointers stay valid.
/// `positions` must have the length of `x`.
unsafe fn match_positions(table: PlainKeys<'_>, x: PlainKeys<'_>, positions: &mut [i32]) -> bool {
    match (table, x) {
        (PlainKeys::Integer(table), PlainKeys::Integer(x)) => {
            if let Some(range) = DenseRange::of(table, table.len().max(x.len())) {
                let mut first: SmallVec<[i32; 32]> = smallvec![0; range.slots()];
                for (index, &value) in table.iter().enumerate().rev() {
                    first[range.slot(value)] = index as i32 + 1;
                }
                for (position, &value) in positions.iter_mut().zip(x) {
                    *position = range.find(value).map_or(0, |slot| first[slot]);
                }
                return true;
            }
            hashed_matches(
                table.len(),
                |index| table[index],
                |index| x[index],
                positions,
            )
        }
        (PlainKeys::Real(table), PlainKeys::Real(x)) => hashed_matches(
            table.len(),
            |index| real_key(table[index]),
            |index| real_key(x[index]),
            positions,
        ),
        (PlainKeys::Interned(table), PlainKeys::Interned(x)) => hashed_matches(
            table.len(),
            |index| table[index],
            |index| x[index],
            positions,
        ),
        _ => false,
    }
}

/// Which elements repeat an earlier one (a later one with `from_last`), or `None` when strings
/// that are not ASCII make pointer equality inexact. `first_only` as for
/// [`duplicated_elements`].
///
/// # Safety
///
/// `keys` must come from [`plain_keys`] on an ordinary (not ALTREP) vector that stays rooted
/// and unmodified for the call, so that its slice and `CHARSXP` pointers stay valid.
unsafe fn duplicate_flags(keys: PlainKeys<'_>, from_last: bool, first_only: bool) -> Option<Bits> {
    match keys {
        PlainKeys::Integer(values) => {
            if let Some(range) = DenseRange::of(values, values.len()) {
                let mut seen = Bits::new(range.slots());
                return Some(visit_in_order(values.len(), from_last, |index| {
                    !seen.insert(range.slot(values[index]))
                }));
            }
            hashed_duplicates(values.len(), from_last, first_only, |index| values[index])
        }
        PlainKeys::Real(values) => {
            // In a vector that never decreases, equal values are neighbours. A NaN fails every
            // comparison, so a vector of two or more that passes holds none, and equal values
            // are then equal keys. Unsorted input fails the test at its first descent.
            if values.windows(2).all(|pair| pair[0] <= pair[1]) {
                return Some(visit_in_order(values.len(), from_last, |index| {
                    let neighbour = if from_last {
                        index + 1
                    } else {
                        index.wrapping_sub(1)
                    };
                    values.get(neighbour) == Some(&values[index])
                }));
            }
            hashed_duplicates(values.len(), from_last, first_only, |index| {
                real_key(values[index])
            })
        }
        PlainKeys::Interned(strings) => {
            hashed_duplicates(strings.len(), from_last, first_only, |index| strings[index])
        }
    }
}

/// The span of a set of integer keys when it is small enough to index an array directly: at
/// most a few slots per element. `NA` gets the slot after the span, so it does not stretch it.
#[derive(Clone, Copy)]
struct DenseRange {
    min: i32,
    span: usize,
}

impl DenseRange {
    fn of(values: &[i32], count: usize) -> Option<Self> {
        let (mut min, mut max) = (i32::MAX, i32::MIN);
        for &value in values {
            if value != NA_INTEGER {
                min = min.min(value);
                max = max.max(value);
            }
        }
        if min > max {
            return Some(Self { min: 0, span: 0 });
        }
        let span = (i64::from(max) - i64::from(min) + 1) as usize;
        (span <= 4 * count + 1024).then_some(Self { min, span })
    }

    fn slots(self) -> usize {
        self.span + 1
    }

    /// The slot of a key known to lie in the range, or to be `NA`.
    fn slot(self, value: i32) -> usize {
        if value == NA_INTEGER {
            self.span
        } else {
            (i64::from(value) - i64::from(self.min)) as usize
        }
    }

    /// The slot of any key, or `None` outside the range.
    fn find(self, value: i32) -> Option<usize> {
        if value == NA_INTEGER {
            return Some(self.span);
        }
        let offset = i64::from(value) - i64::from(self.min);
        (offset >= 0 && (offset as usize) < self.span).then_some(offset as usize)
    }
}

/// `false`, with `out` partly written, when a key that is not [`TableKey::exact`] is a distinct
/// table key or misses the table, because then equal values may differ as keys.
fn hashed_matches<K: TableKey + Eq>(
    table_count: usize,
    table_key: impl Fn(usize) -> K,
    x_key: impl Fn(usize) -> K,
    out: &mut [i32],
) -> bool {
    let mut table = PositionTable::new(table_count);
    for index in 0..table_count {
        let key = table_key(index);
        if table.find_or_insert(key, index, &table_key).is_none() && !key.exact() {
            return false;
        }
    }
    for (index, position) in out.iter_mut().enumerate() {
        let key = x_key(index);
        *position = match table.find(key, &table_key) {
            Some(found) => found as i32 + 1,
            None if key.exact() => 0,
            None => return false,
        };
    }
    true
}

/// `None` when a distinct key is not [`TableKey::exact`]. Each word of flags is built in a
/// register over its 64 elements and stored once. With `first_only` the scan stops after the
/// first word, in visiting order, that holds a duplicate, which is all `anyDuplicated()` reads.
fn hashed_duplicates<K: TableKey + Eq>(
    count: usize,
    from_last: bool,
    first_only: bool,
    key_at: impl Fn(usize) -> K,
) -> Option<Bits> {
    let mut table = PositionTable::new(count);
    let mut duplicated = Bits::new(count);
    let words = duplicated.words.len();
    for step in 0..words {
        let word_index = if from_last { words - 1 - step } else { step };
        let base = word_index * 64;
        let span = base..(base + 64).min(count);
        let mut word = 0;
        if from_last {
            for index in span.rev() {
                word |= duplicate_bit(&mut table, &key_at, index, base)?;
            }
        } else {
            for index in span {
                word |= duplicate_bit(&mut table, &key_at, index, base)?;
            }
        }
        duplicated.words[word_index] = word;
        if first_only && word != 0 {
            break;
        }
    }
    Some(duplicated)
}

/// Element `index`'s bit in the word starting at `base`, set when it repeats a key already in
/// `table`, or `None` for a new key that is not [`TableKey::exact`]. Always inlined, because a
/// call per element costs more than the probe.
#[inline(always)]
fn duplicate_bit<K: TableKey + Eq>(
    table: &mut PositionTable,
    key_at: &impl Fn(usize) -> K,
    index: usize,
    base: usize,
) -> Option<u64> {
    let key = key_at(index);
    match table.find_or_insert(key, index, key_at) {
        Some(_) => Some(1 << (index - base)),
        None if key.exact() => Some(0),
        None => None,
    }
}

/// `flag(index)` for each of `count` indices, visited first to last or, with `from_last`, last
/// to first.
fn visit_in_order(count: usize, from_last: bool, mut flag: impl FnMut(usize) -> bool) -> Bits {
    let mut flags = Bits::new(count);
    if from_last {
        for index in (0..count).rev() {
            if flag(index) {
                flags.insert(index);
            }
        }
    } else {
        for index in 0..count {
            if flag(index) {
                flags.insert(index);
            }
        }
    }
    flags
}

/// A set of indices below `len`, one bit each.
pub(crate) struct Bits {
    words: Vec<u64>,
    len: usize,
}

impl Bits {
    fn new(len: usize) -> Self {
        Self {
            words: vec![0; len.div_ceil(64)],
            len,
        }
    }

    /// Adds `index`, answering whether it was absent.
    fn insert(&mut self, index: usize) -> bool {
        let Some(word) = self.words.get_mut(index / 64) else {
            return false;
        };
        let bit = 1 << (index % 64);
        let absent = *word & bit == 0;
        *word |= bit;
        absent
    }

    fn count(&self) -> usize {
        self.words
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    pub(crate) fn first(&self) -> Option<usize> {
        let (index, word) = self
            .words
            .iter()
            .enumerate()
            .find(|(_, word)| **word != 0)?;
        Some(index * 64 + word.trailing_zeros() as usize)
    }

    fn last(&self) -> Option<usize> {
        let (index, word) = self
            .words
            .iter()
            .enumerate()
            .rfind(|(_, word)| **word != 0)?;
        Some(index * 64 + 63 - word.leading_zeros() as usize)
    }

    /// The indices below `len` not in the set, in increasing order.
    fn clear_positions(&self) -> impl Iterator<Item = usize> + '_ {
        let len = self.len;
        self.words
            .iter()
            .enumerate()
            .flat_map(move |(index, &word)| {
                let mut remaining = !word;
                std::iter::from_fn(move || {
                    (remaining != 0).then(|| {
                        let bit = remaining.trailing_zeros() as usize;
                        remaining &= remaining - 1;
                        index * 64 + bit
                    })
                })
            })
            .take_while(move |&position| position < len)
    }
}

/// A key that `PositionTable` can hash. The hash must spread every input bit into the high bits,
/// which pick the slot.
trait TableKey: Copy {
    fn table_hash(self) -> u64;

    /// Whether equal keys are always equal values, so that a key missing from the table is a
    /// value missing from it. Only interned strings can fail this (see [`PlainKeys`]).
    fn exact(self) -> bool {
        true
    }
}

/// A multiply after folding the high half in, so that keys differing only in their high bits,
/// such as whole-number doubles, still land in different slots.
fn mix(bits: u64) -> u64 {
    (bits ^ (bits >> 32)).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

impl TableKey for i32 {
    fn table_hash(self) -> u64 {
        mix(u64::from(self as u32))
    }
}

impl TableKey for u64 {
    fn table_hash(self) -> u64 {
        mix(self)
    }
}

/// A list element with its [`structural_hash`]. Two are the same key when the hashes agree and
/// the elements are `identical()` under the default flags, which the caller tests, because
/// `identical` needs the list rooted.
#[derive(Clone, Copy)]
struct ListElement {
    hash: u64,
    element: Sexp,
}

impl TableKey for ListElement {
    fn table_hash(self) -> u64 {
        mix(self.hash)
    }
}

impl TableKey for Sexp {
    fn table_hash(self) -> u64 {
        mix(self.as_ptr() as usize as u64 >> 3)
    }

    fn exact(self) -> bool {
        // SAFETY: the key is an element of a live character vector, so a valid string object.
        unsafe { self.has_gp(gp::ASCII) }
    }
}

/// An open-addressing hash table from keys to the first position that holds them. A slot packs
/// the 1-based position (0 is empty) in its low half and the hash's high 32 bits in its high
/// half, leaving the keys in their vector: a probe reads a stored key only when the hash bits
/// agree, so a miss on a large table costs one random read rather than one per occupied slot,
/// and growing rehashes from the slots alone. Probing is linear over a power-of-two array kept
/// at most half full, and the slot comes from the hash's high bits. The table starts with two
/// slots per element, so a vector of up to [`PositionTable::PRESIZED`] / 2 elements never
/// rehashes, and doubles past that cap only as distinct keys arrive: a million strings with
/// four distinct values zero 512 KB once and probe a few cache lines of it, where sizing by
/// elements would take 16 MB.
struct PositionTable {
    slots: SmallVec<[u64; 16]>,
    /// The table's slot index is the top `32 - shift` bits of the 32 stored hash bits.
    shift: u32,
    /// Inserts left before the table is more than half full, counted down so that an insert
    /// tests one field rather than the slots' length, which `SmallVec` keeps behind a branch.
    room: usize,
    /// Lookups so far and the most keys the table can receive, from which growing extrapolates
    /// the distinct keys still to come.
    lookups: usize,
    capacity: usize,
}

impl PositionTable {
    /// The most slots a table starts with. Growing by doubling zeroes each new array and moves
    /// every stored slot, which costs more than zeroing this many slots up front.
    const PRESIZED: usize = 1 << 16;

    fn new(count: usize) -> Self {
        let size = (2 * count).clamp(32, Self::PRESIZED).next_power_of_two();
        Self {
            slots: smallvec![0; size],
            shift: 32 - size.trailing_zeros(),
            room: size / 2 + 1,
            lookups: 0,
            capacity: count,
        }
    }

    /// The position of the first key equal to `key`, or `None` after recording `position` for
    /// it.
    fn find_or_insert<K: TableKey + Eq>(
        &mut self,
        key: K,
        position: usize,
        key_at: impl Fn(usize) -> K,
    ) -> Option<usize> {
        self.find_or_insert_by(key, position, key_at, |left, right| left == right)
    }

    /// [`PositionTable::find_or_insert`] with keys compared by `same`.
    fn find_or_insert_by<K: TableKey>(
        &mut self,
        key: K,
        position: usize,
        key_at: impl Fn(usize) -> K,
        same: impl Fn(K, K) -> bool,
    ) -> Option<usize> {
        let slots: &mut [u64] = &mut self.slots;
        let mask = slots.len() - 1;
        let tag = key.table_hash() >> 32;
        let mut slot = (tag >> self.shift) as usize;
        self.lookups += 1;
        loop {
            let stored = slots[slot & mask];
            if stored == 0 {
                break;
            }
            if stored >> 32 == tag {
                let found = (stored as u32 - 1) as usize;
                if same(key_at(found), key) {
                    return Some(found);
                }
            }
            slot += 1;
        }
        slots[slot & mask] = tag << 32 | (position as u64 + 1);
        self.room -= 1;
        if self.room == 0 {
            self.grow();
        }
        None
    }

    fn find<K: TableKey + Eq>(&self, key: K, key_at: impl Fn(usize) -> K) -> Option<usize> {
        let slots: &[u64] = &self.slots;
        let mask = slots.len() - 1;
        let tag = key.table_hash() >> 32;
        let mut slot = (tag >> self.shift) as usize;
        loop {
            let stored = slots[slot & mask];
            if stored == 0 {
                return None;
            }
            if stored >> 32 == tag {
                let found = (stored as u32 - 1) as usize;
                if key_at(found) == key {
                    return Some(found);
                }
            }
            slot += 1;
        }
    }

    /// Moves every stored slot, by its stored hash bits, into a table at least twice as large:
    /// large enough for the distinct keys that the share of distinct keys so far predicts for
    /// the remaining lookups, so that a vector of distinct values grows a few times rather than
    /// once per doubling, each of which zeroes a new array and moves every slot again. A step
    /// grows at most eightfold, because `anyDuplicated` often stops long before the end and
    /// would otherwise zero a table sized for the whole vector.
    #[cold]
    fn grow(&mut self) {
        let distinct = self.slots.len() / 2;
        let predicted = distinct
            .saturating_mul(self.capacity)
            .checked_div(self.lookups)
            .unwrap_or(0)
            .min(self.capacity);
        let size = (2 * predicted)
            .next_power_of_two()
            .clamp(2 * self.slots.len(), 8 * self.slots.len());
        let bigger = smallvec![0; size];
        let old = std::mem::replace(&mut self.slots, bigger);
        self.shift -= size.trailing_zeros() - old.len().trailing_zeros();
        let slots: &mut [u64] = &mut self.slots;
        let mask = slots.len() - 1;
        let mut moved = 0;
        for stored in old.into_iter().filter(|&stored| stored != 0) {
            let mut slot = (stored >> 32 >> self.shift) as usize;
            while slots[slot & mask] != 0 {
                slot += 1;
            }
            slots[slot & mask] = stored;
            moved += 1;
        }
        self.room = slots.len() / 2 + 1 - moved;
    }
}

/// A hash consistent with `identical()` under its default flags: identical objects hash alike.
/// Attributes and the insides of functions and environments are left out, which only costs
/// collisions.
unsafe fn structural_hash(object: Sexp) -> u64 {
    let mut hasher = FxHasher::default();
    hash_object(object, &mut hasher);
    hasher.finish()
}

unsafe fn hash_object(object: Sexp, hasher: &mut FxHasher) {
    object.kind().code().hash(hasher);
    match object.kind() {
        SexpType::Logical | SexpType::Integer => values::ints(object).hash(hasher),
        SexpType::Real => {
            for &value in values::reals(object) {
                real_key(value).hash(hasher);
            }
        }
        SexpType::Complex => {
            for value in values::complexes(object) {
                real_key(value.r).hash(hasher);
                real_key(value.i).hash(hasher);
            }
        }
        SexpType::String => {
            for index in 0..length(object) {
                string_at(object, index).hash(hasher);
            }
        }
        SexpType::Raw => values::raws(object).hash(hasher),
        SexpType::List | SexpType::Expression => {
            for index in 0..length(object) {
                hash_object(object.element(index), hasher);
            }
        }
        SexpType::Pairlist | SexpType::Language => {
            let mut cell = object;
            while matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) {
                hash_object(cell.car(), hasher);
                cell = cell.cdr();
            }
        }
        SexpType::Symbol => object.hash(hasher),
        _ => {}
    }
}

/// `x` or `table` of `match`, with factors as their labels and other classed objects as
/// `mtfrm()` makes them. The transform comes before the vector check, as in GNU R, so an S4
/// object with an `as.character` method matches as its strings.
unsafe fn match_operand(object: Sexp, call: &Call<'_>) -> R<Sexp> {
    let operand = if !object.is_object() {
        object
    } else if values::inherits(object, "factor") {
        factor_as_character(object)?
    } else {
        let transform = values::call(values::install("mtfrm")?, &[object])?;
        eval::eval(transform, call.env()?)?
    };
    if operand != nil() && !operand.kind().is_vector() {
        return Err(error("'match' requires vector arguments"));
    }
    Ok(operand)
}

unsafe fn identical_skipping(x: Sexp, y: Sexp, flags: &IdenticalFlags, skipped: &[Sexp]) -> bool {
    if x == y {
        return true;
    }
    let kind = x.kind();
    if kind != y.kind()
        || x.is_object() != y.is_object()
        || x.is_array() != y.is_array()
        || x.has_gp(gp::S4_OBJECT) != y.has_gp(gp::S4_OBJECT)
    {
        return false;
    }
    let srcref = [symbols().srcref];
    let skipped = if kind == SexpType::Closure && flags.ignore_srcref {
        &srcref[..]
    } else {
        skipped
    };
    if !attributes_identical(x, y, flags, skipped) {
        return false;
    }
    match kind {
        SexpType::Nil => true,
        SexpType::Logical | SexpType::Integer => values::ints(x) == values::ints(y),
        SexpType::Real => {
            let (left, right) = (values::reals(x), values::reals(y));
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(&a, &b)| reals_identical(a, b, flags))
        }
        SexpType::Complex => {
            let (left, right) = (values::complexes(x), values::complexes(y));
            left.len() == right.len()
                && left.iter().zip(right).all(|(a, b)| {
                    reals_identical(a.r, b.r, flags) && reals_identical(a.i, b.i, flags)
                })
        }
        SexpType::String => {
            length(x) == length(y)
                && (0..length(x))
                    .all(|index| values::same_string(x.element(index), y.element(index)))
        }
        SexpType::Raw => values::raws(x) == values::raws(y),
        SexpType::I64 => x.slice::<i64>() == y.slice::<i64>(),
        SexpType::List | SexpType::Expression => {
            length(x) == length(y)
                && (0..length(x)).all(|index| identical(x.element(index), y.element(index), flags))
        }
        SexpType::Pairlist | SexpType::Language | SexpType::Dots => {
            let (mut left, mut right) = (x, y);
            loop {
                let left_cell = matches!(
                    left.kind(),
                    SexpType::Pairlist | SexpType::Language | SexpType::Dots
                );
                let right_cell = matches!(
                    right.kind(),
                    SexpType::Pairlist | SexpType::Language | SexpType::Dots
                );
                if !left_cell || !right_cell {
                    return left_cell == right_cell;
                }
                if left.tag() != right.tag() || !identical(left.car(), right.car(), flags) {
                    return false;
                }
                left = left.cdr();
                right = right.cdr();
            }
        }
        SexpType::Closure => {
            let source = [symbols().srcref, symbols().srcfile, symbols().whole_srcref];
            let body_skipped: &[Sexp] = if flags.ignore_srcref { &source } else { &[] };
            identical(x.car(), y.car(), flags)
                && identical_skipping(
                    crate::bytecode::closure_body(x),
                    crate::bytecode::closure_body(y),
                    flags,
                    body_skipped,
                )
                && (flags.ignore_environment || x.tag() == y.tag())
        }
        // Promises match when they would evaluate the same code in the same environment, so two
        // `...` objects built from the same call site compare equal.
        SexpType::Promise => {
            x.tag() == y.tag()
                && identical(x.car(), y.car(), flags)
                && identical(x.cdr(), y.cdr(), flags)
        }
        SexpType::Builtin | SexpType::Special => x.primitive_index() == y.primitive_index(),
        SexpType::ExternalPtr => {
            !flags.external_pointer_as_reference && x.external_address() == y.external_address()
        }
        SexpType::Char => values::same_string(x, y),
        SexpType::Object => true,
        SexpType::Record
            if matches!(records::native_kind(x), NativeKind::Dict | NativeKind::Set) =>
        {
            x.record_layout() == y.record_layout()
                && standard_library::collections_identical(x, y, flags)
        }
        // Two records over the same typed-core value are identical without filling either.
        SexpType::Record if x.is_lazy_record() || y.is_lazy_record() => {
            (x.is_lazy_record()
                && y.is_lazy_record()
                && x.lazy_record_value() == y.lazy_record_value())
                || (x.record_layout() == y.record_layout() && {
                    records::settle_uncollected(x);
                    records::settle_uncollected(y);
                    identical(x, y, flags)
                })
        }
        SexpType::Record => {
            x.record_layout() == y.record_layout()
                && (0..x.record_slot_count()).all(|index| {
                    match (x.record_slot(index), y.record_slot(index)) {
                        (Slot::Handle(left), Slot::Handle(right)) => identical(left, right, flags),
                        (Slot::Real(left), Slot::Real(right)) => {
                            reals_identical(left, right, flags)
                        }
                        (Slot::Integer(left), Slot::Integer(right))
                        | (Slot::Logical(left), Slot::Logical(right)) => left == right,
                        // A scalar is boxed only when it could not be stored raw (an ALTREP value, or
                        // a double whose bits collide with the raw encoding), so compare contents.
                        (Slot::Handle(handle), raw) | (raw, Slot::Handle(handle)) => {
                            raw_slot_identical(raw, handle, flags)
                        }
                        _ => false,
                    }
                })
        }
        _ => false,
    }
}

unsafe fn raw_slot_identical(raw: Slot, handle: Sexp, flags: &IdenticalFlags) -> bool {
    if handle.attrib() != nil() || length(handle) != 1 {
        return false;
    }
    let Ok(handle) = values::materialize(handle) else {
        return false;
    };
    match (raw, handle.kind()) {
        (Slot::Real(value), SexpType::Real) => {
            reals_identical(value, values::reals(handle)[0], flags)
        }
        (Slot::Integer(value), SexpType::Integer) | (Slot::Logical(value), SexpType::Logical) => {
            values::ints(handle)[0] == value
        }
        _ => false,
    }
}

/// Compares attribute lists, as a set by default or in order, leaving out the tags in
/// `skipped`.
unsafe fn attributes_identical(x: Sexp, y: Sexp, flags: &IdenticalFlags, skipped: &[Sexp]) -> bool {
    if x.attrib() == nil() && y.attrib() == nil() {
        return true;
    }
    let items = |object: Sexp| {
        std::iter::successors(Some(object.attrib()), |cell: &Sexp| Some(cell.cdr()))
            .take_while(|&cell| cell != nil())
            .map(|cell| (cell.tag(), cell.car()))
            .filter(|(tag, _)| !skipped.contains(tag))
    };
    if items(x).count() != items(y).count() {
        return false;
    }
    let same = |tag: Sexp, value: Sexp, other: Sexp| {
        if tag == symbols().row_names {
            row_names_identical(value, other, flags)
        } else {
            identical(value, other, flags)
        }
    };
    if flags.attributes_as_set {
        items(x).all(|(tag, value)| {
            items(y).any(|(other_tag, other)| other_tag == tag && same(tag, value, other))
        })
    } else {
        items(x)
            .zip(items(y))
            .all(|((tag, value), (other_tag, other))| tag == other_tag && same(tag, value, other))
    }
}

/// GNU R compares row names as `attr(x, "row.names")` reads them, so the compact `c(NA, n)`
/// equals the integers `1:n` it stands for.
unsafe fn row_names_identical(left: Sexp, right: Sexp, flags: &IdenticalFlags) -> bool {
    let compact = values::compact_row_count;
    let counts_up = |row_names: Sexp, count: usize| {
        row_names.kind() == SexpType::Integer
            && values::ints(row_names)
                .iter()
                .copied()
                .eq((1..=count).map(|row| row as i32))
    };
    match (compact(left), compact(right)) {
        (Some(left), Some(right)) => left == right,
        (Some(count), None) => counts_up(right, count),
        (None, Some(count)) => counts_up(left, count),
        (None, None) => identical(left, right, flags),
    }
}

/// Two doubles under `identical()`'s `num.eq` and `single.NA`.
fn reals_identical(left: f64, right: f64, flags: &IdenticalFlags) -> bool {
    if flags.single_na {
        if is_na_real(left) || is_na_real(right) {
            return is_na_real(left) && is_na_real(right);
        }
        if left.is_nan() || right.is_nan() {
            return left.is_nan() && right.is_nan();
        }
        if flags.num_eq {
            left == right
        } else {
            left.to_bits() == right.to_bits()
        }
    } else if flags.num_eq && !left.is_nan() && !right.is_nan() {
        left == right
    } else {
        left.to_bits() == right.to_bits()
    }
}

/// `.Internal(merge(xinds, yinds, all.x, all.y))`: `merge.data.frame` codes each row by its key's
/// position among the keys both sides share (0 for none), and this pairs the rows with equal
/// codes, in order of the code. With `all.x` (`all.y`) the rows of that side that matched
/// nothing are listed too.
unsafe fn do_merge(call: &Call<'_>) -> R<Sexp> {
    let x_codes = call.arg(0);
    let y_codes = call.arg(1);
    if x_codes.kind() != SexpType::Integer || length(x_codes) == 0 {
        return Err(call.error("invalid 'xinds' argument"));
    }
    if y_codes.kind() != SexpType::Integer || length(y_codes) == 0 {
        return Err(call.error("invalid 'yinds' argument"));
    }
    let all_x = as_logical_scalar(call.arg(2));
    if all_x == NA_LOGICAL {
        return Err(call.error("'all.x' must be TRUE or FALSE"));
    }
    let all_y = as_logical_scalar(call.arg(3));
    if all_y == NA_LOGICAL {
        return Err(call.error("'all.y' must be TRUE or FALSE"));
    }
    // Both sides sorted by code with GNU R's Shell sort, which is not stable: the order of the
    // rows within a code is the order `merge()` returns them in, so it must be the same sort.
    let (x, x_rows_sorted) = shell_sort_with_rows(values::ints(x_codes));
    let (y, y_rows_sorted) = shell_sort_with_rows(values::ints(y_codes));
    let x_lone = x.partition_point(|&code| code <= 0);
    let y_lone = y.partition_point(|&code| code <= 0);
    // The rows of each code that both sides share, as a range of each sorted side.
    let mut groups = Vec::new();
    let (mut i, mut j) = (x_lone, y_lone);
    while i < x.len() {
        let code = x[i];
        let x_end = i + x[i..].partition_point(|&other| other == code);
        j += y[j..].partition_point(|&other| other < code);
        let y_end = j + y[j..].partition_point(|&other| other == code);
        groups.push((i..x_end, j..y_end));
        i = x_end;
        j = y_end;
    }
    let pairs: usize = groups.iter().map(|(xs, ys)| xs.len() * ys.len()).sum();
    let x_rows = alloc(SexpType::Integer, pairs)?;
    crate::heap().root(x_rows);
    let y_rows = alloc(SexpType::Integer, pairs)?;
    crate::heap().root(y_rows);
    {
        let x_slots = values::ints_mut(x_rows);
        let y_slots = values::ints_mut(y_rows);
        let mut next = 0;
        for (xs, ys) in groups {
            for x_index in xs {
                for y_index in ys.clone() {
                    x_slots[next] = x_rows_sorted[x_index];
                    y_slots[next] = y_rows_sorted[y_index];
                    next += 1;
                }
            }
        }
    }
    let x_alone = if all_x == 1 {
        values::integer_vector(&x_rows_sorted[..x_lone])?
    } else {
        nil()
    };
    crate::heap().root(x_alone);
    let y_alone = if all_y == 1 {
        values::integer_vector(&y_rows_sorted[..y_lone])?
    } else {
        nil()
    };
    crate::heap().root(y_alone);
    values::named_list(&[
        ("xi", x_rows),
        ("yi", y_rows),
        ("x.alone", x_alone),
        ("y.alone", y_alone),
    ])
}

/// GNU R's `isort_with_index`: `codes` sorted by Shell sort with the gaps 1, 4, 13, ..., with
/// the 1-based row each sorted code came from.
fn shell_sort_with_rows(codes: &[i32]) -> (Vec<i32>, Vec<i32>) {
    let mut sorted = codes.to_vec();
    let mut rows: Vec<i32> = (1..=codes.len() as i32).collect();
    let count = sorted.len();
    let mut gap = 1;
    while gap <= count / 9 {
        gap = 3 * gap + 1;
    }
    while gap > 0 {
        for start in gap..count {
            let (code, row) = (sorted[start], rows[start]);
            let mut index = start;
            while index >= gap && sorted[index - gap] > code {
                sorted[index] = sorted[index - gap];
                rows[index] = rows[index - gap];
                index -= gap;
            }
            sorted[index] = code;
            rows[index] = row;
        }
        gap /= 3;
    }
    (sorted, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The printable-ASCII shortcut must order exactly as ICU4X's collator does for every locale
    /// it is used for, including strings that are prefixes of each other or differ only in case.
    #[test]
    fn ascii_collation_matches_icu() {
        let printable: Vec<u8> = (b' '..=b'~').collect();
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut texts: Vec<String> = Vec::new();
        for &first in &printable {
            texts.push(String::from(first as char));
            for &second in &printable {
                texts.push(String::from_iter([first as char, second as char]));
            }
        }
        for _ in 0..4000 {
            let length = (next() % 6) as usize;
            let alphabet = if next() % 2 == 0 {
                &b"aAbB1 -_."[..]
            } else {
                &printable[..]
            };
            texts.push(
                (0..length)
                    .map(|_| alphabet[(next() % alphabet.len() as u64) as usize] as char)
                    .collect(),
            );
        }
        for language in ["und", "en", "en-US", "de", "fr", "fr-CA", "it", "nl", "pt"] {
            let locale = Locale::try_from_str(language).expect("a valid tag");
            let preferences = CollatorPreferences::from(&locale);
            assert!(root_ascii_order(&locale, &preferences), "{language}");
            let collator = CollatorBorrowed::try_new(preferences, CollatorOptions::default())
                .expect("compiled data");
            for (index, left) in texts.iter().enumerate() {
                for right in texts.iter().skip(index % 397).step_by(397) {
                    assert_eq!(
                        ascii_collate(left.as_bytes(), right.as_bytes()),
                        collator.compare(left, right),
                        "{language}: {left:?} and {right:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn locale_names_become_icu_tags() {
        assert_eq!(icu_locale_tag("de_DE.UTF-8").0, "de-DE");
        assert_eq!(icu_locale_tag("C.UTF-8").0, "C");
        assert_eq!(
            icu_locale_tag("de@collation=phonebook"),
            (
                "de-u-co-phonebk".to_owned(),
                "de@collation=phonebook".to_owned()
            )
        );
    }
}
