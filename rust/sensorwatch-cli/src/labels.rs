//! The shared JSON `type` vocabulary for reading categories.

use sensorwatch::ReadingType;

/// The JSON `type` label for a reading category — the Title-case vocabulary
/// the Python tooling uses (`SENSOR_TYPES` in `sensorwatch/hwinfo_shm.py`).
pub(crate) fn type_label(kind: ReadingType) -> &'static str {
    match kind {
        ReadingType::None => "None",
        ReadingType::Temperature => "Temperature",
        ReadingType::Voltage => "Voltage",
        ReadingType::Fan => "Fan",
        ReadingType::Current => "Current",
        ReadingType::Power => "Power",
        ReadingType::Clock => "Clock",
        ReadingType::Usage => "Usage",
        ReadingType::Other => "Other",
        // `ReadingType` is #[non_exhaustive]; `Unknown` and any future variant
        // fold to the same bare "unknown" the Python helper emits.
        _ => "unknown",
    }
}

/// The nine canonical Title-case labels; the tenth member of the vocabulary
/// is the bare `"unknown"` everything unrecognized folds to.
pub(crate) const CANONICAL_LABELS: [&str; 9] = [
    "None",
    "Temperature",
    "Voltage",
    "Fan",
    "Current",
    "Power",
    "Clock",
    "Usage",
    "Other",
];

/// Fold an arbitrary stream or config `type` string onto the canonical label
/// set: the nine Title-case labels map to themselves (case-insensitively, so
/// rule configs may write `"voltage"`); everything else — including the
/// Python logger's historical `unknown(<N>)` forms — folds to the same bare
/// `"unknown"` that [`type_label`] emits for unrecognized codes.
pub(crate) fn normalize_type_label(raw: &str) -> &'static str {
    for label in CANONICAL_LABELS {
        if raw.eq_ignore_ascii_case(label) {
            return label;
        }
    }
    "unknown"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_labels_match_the_python_vocabulary() {
        assert_eq!(type_label(ReadingType::None), "None");
        assert_eq!(type_label(ReadingType::Temperature), "Temperature");
        assert_eq!(type_label(ReadingType::Voltage), "Voltage");
        assert_eq!(type_label(ReadingType::Fan), "Fan");
        assert_eq!(type_label(ReadingType::Current), "Current");
        assert_eq!(type_label(ReadingType::Power), "Power");
        assert_eq!(type_label(ReadingType::Clock), "Clock");
        assert_eq!(type_label(ReadingType::Usage), "Usage");
        assert_eq!(type_label(ReadingType::Other), "Other");
        assert_eq!(type_label(ReadingType::Unknown), "unknown");
    }

    #[test]
    fn normalize_is_identity_on_canonical_labels() {
        for label in [
            "None",
            "Temperature",
            "Voltage",
            "Fan",
            "Current",
            "Power",
            "Clock",
            "Usage",
            "Other",
        ] {
            assert_eq!(normalize_type_label(label), label);
        }
    }

    #[test]
    fn normalize_is_case_insensitive() {
        assert_eq!(normalize_type_label("voltage"), "Voltage");
        assert_eq!(normalize_type_label("TEMPERATURE"), "Temperature");
        assert_eq!(normalize_type_label("none"), "None");
    }

    #[test]
    fn normalize_folds_unknown_forms() {
        // The bare label, the Python logger's historical parameterized form,
        // and arbitrary garbage all fold to the canonical bare "unknown".
        assert_eq!(normalize_type_label("unknown"), "unknown");
        assert_eq!(normalize_type_label("Unknown"), "unknown");
        assert_eq!(normalize_type_label("unknown(35)"), "unknown");
        assert_eq!(normalize_type_label("Voltage "), "unknown");
        assert_eq!(normalize_type_label(""), "unknown");
        assert_eq!(normalize_type_label("garbage"), "unknown");
    }
}
