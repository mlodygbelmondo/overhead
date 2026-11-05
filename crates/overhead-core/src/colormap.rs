//! "Inferno"-like colour map for spectrogram display.

const STOPS: [(f32, [u8; 3]); 9] = [
    (0.000, [0, 0, 4]),
    (0.125, [22, 11, 57]),
    (0.250, [66, 10, 104]),
    (0.375, [106, 23, 110]),
    (0.500, [147, 38, 103]),
    (0.625, [188, 55, 84]),
    (0.750, [221, 81, 58]),
    (0.875, [245, 125, 21]),
    (1.000, [252, 255, 164]),
];

/// RGB for `x` in [0, 1].
pub fn inferno(x: f32) -> [u8; 3] {
    let x = x.clamp(0.0, 1.0);
    let i = STOPS.iter().position(|(p, _)| *p >= x).unwrap_or(STOPS.len() - 1).max(1);
    let (p0, c0) = STOPS[i - 1];
    let (p1, c1) = STOPS[i];
    let t = if p1 > p0 { (x - p0) / (p1 - p0) } else { 0.0 };
    std::array::from_fn(|k| (c0[k] as f32 + (c1[k] as f32 - c0[k] as f32) * t).round() as u8)
}

/// 256-entry lookup table.
pub fn lut() -> [[u8; 3]; 256] {
    std::array::from_fn(|i| inferno(i as f32 / 255.0))
}
