/// Clamps to `[0.0, 1.0]`, treating `NaN` as `0.0`, then canonicalizes
/// `-0.0` to `+0.0` so bit-pattern equality and hashing stay stable.
pub(crate) fn clamp_unit(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0) + 0.0
    }
}
