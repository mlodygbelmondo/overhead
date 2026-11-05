//! Aircraft class from the ICAO type designator (ICAO Doc 8643 description
//! such as `L2P` = landplane, two piston engines).

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AircraftClass {
    Piston,
    Turboprop,
    /// Helicopters and gyroplanes.
    Rotor,
    Electric,
    Jet,
    /// Gliders, balloons, airships: no propeller tone worth matching.
    Unpowered,
    /// No (known) type code. Kept as a candidate, like the reference method
    /// which only removed known jets.
    Unknown,
}

impl AircraftClass {
    /// Plausible source of a propeller/rotor Doppler line.
    pub fn is_candidate(self) -> bool {
        matches!(self, Self::Piston | Self::Turboprop | Self::Rotor | Self::Electric | Self::Unknown)
    }
}

static TABLE: &str = include_str!("../data/aircraft_types.tsv");

fn table() -> &'static HashMap<&'static str, (&'static str, &'static str)> {
    static MAP: OnceLock<HashMap<&'static str, (&'static str, &'static str)>> = OnceLock::new();
    MAP.get_or_init(|| {
        TABLE
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .filter_map(|l| {
                let mut it = l.split('\t');
                Some((it.next()?, (it.next().unwrap_or(""), it.next().unwrap_or(""))))
            })
            .collect()
    })
}

/// ICAO description (e.g. `L1P`) of a type designator.
pub fn description(type_code: &str) -> Option<&'static str> {
    table().get(type_code.trim().to_ascii_uppercase().as_str()).map(|v| v.0).filter(|d| !d.is_empty())
}

/// Classify a type designator; `category` is the ADS-B emitter category
/// (`A1`..`A7`, `B1`..) used when the type is unknown.
pub fn classify(type_code: Option<&str>, category: Option<&str>) -> AircraftClass {
    if let Some(t) = type_code.filter(|t| !t.trim().is_empty()) {
        let t = t.trim().to_ascii_uppercase();
        if t == "GLID" || t == "BALL" || t == "SHIP" {
            return AircraftClass::Unpowered;
        }
        if let Some(desc) = description(&t) {
            let b = desc.as_bytes();
            let body = b.first().copied().unwrap_or(b'L');
            let engine = b.get(2).copied().unwrap_or(b'-');
            if body == b'-' {
                return AircraftClass::Unknown;
            }
            if body == b'H' || body == b'G' {
                return AircraftClass::Rotor;
            }
            return match engine {
                b'P' => AircraftClass::Piston,
                b'T' => AircraftClass::Turboprop,
                b'J' => AircraftClass::Jet,
                b'E' => AircraftClass::Electric,
                b'-' => AircraftClass::Unpowered,
                _ => AircraftClass::Unknown,
            };
        }
    }
    match category.map(|c| c.trim().to_ascii_uppercase()) {
        Some(c) if c == "A7" => AircraftClass::Rotor,
        Some(c) if c == "A3" || c == "A4" || c == "A5" || c == "A6" => AircraftClass::Jet,
        Some(c) if c == "B1" || c == "B2" => AircraftClass::Unpowered,
        _ => AircraftClass::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_types() {
        assert_eq!(classify(Some("C172"), None), AircraftClass::Piston);
        assert_eq!(classify(Some("AT76"), None), AircraftClass::Turboprop);
        assert_eq!(classify(Some("DH8D"), None), AircraftClass::Turboprop);
        assert_eq!(classify(Some("EC35"), None), AircraftClass::Rotor);
        assert_eq!(classify(Some("R44"), None), AircraftClass::Rotor);
        assert_eq!(classify(Some("A320"), None), AircraftClass::Jet);
        assert_eq!(classify(Some("B77W"), None), AircraftClass::Jet);
        assert_eq!(classify(Some("GLID"), None), AircraftClass::Unpowered);
        assert_eq!(classify(None, Some("A7")), AircraftClass::Rotor);
        assert_eq!(classify(None, Some("A1")), AircraftClass::Unknown);
        assert_eq!(classify(Some("ZZZZ"), None), AircraftClass::Unknown);
    }
}
