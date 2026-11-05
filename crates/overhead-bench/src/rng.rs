//! Deterministic random numbers (SplitMix64). Every scenario element draws from
//! its own forked stream, so a scene depends only on the seed and adding one
//! source does not reshuffle the others.

#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut r = Self(seed ^ 0x6A09_E667_F3BC_C909);
        r.next_u64();
        r
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Independent child stream.
    pub fn fork(&mut self) -> Rng {
        Rng::new(self.next_u64())
    }

    /// Uniform in [0, 1).
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.f64()
    }

    /// Log-uniform in [lo, hi] (scale parameters such as distances).
    pub fn log_range(&mut self, lo: f64, hi: f64) -> f64 {
        (lo.ln() + (hi.ln() - lo.ln()) * self.f64()).exp()
    }

    /// Uniform integer in [lo, hi] (inclusive).
    pub fn int(&mut self, lo: usize, hi: usize) -> usize {
        lo + (self.next_u64() % (hi - lo + 1) as u64) as usize
    }

    pub fn chance(&mut self, p: f64) -> bool {
        self.f64() < p
    }

    pub fn sign(&mut self) -> f64 {
        if self.chance(0.5) { 1.0 } else { -1.0 }
    }

    pub fn gaussian(&mut self) -> f64 {
        let u1 = self.f64().max(1e-300);
        let u2 = self.f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// Pair of independent standard normals (Box–Muller, both outputs).
    pub fn gaussian2(&mut self) -> (f64, f64) {
        let u1 = self.f64().max(1e-300);
        let (s, c) = (std::f64::consts::TAU * self.f64()).sin_cos();
        let r = (-2.0 * u1.ln()).sqrt();
        (r * c, r * s)
    }

    /// Zero-mean uniform noise with unit variance (cheap white noise).
    pub fn white(&mut self) -> f64 {
        (self.f64() * 2.0 - 1.0) * 3f64.sqrt()
    }

    /// Index drawn with probability proportional to `weights`.
    pub fn weighted(&mut self, weights: &[f64]) -> usize {
        let total: f64 = weights.iter().sum();
        let mut x = self.f64() * total;
        for (i, w) in weights.iter().enumerate() {
            if x < *w {
                return i;
            }
            x -= w;
        }
        weights.len() - 1
    }
}
