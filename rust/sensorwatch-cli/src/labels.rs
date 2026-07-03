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
}
