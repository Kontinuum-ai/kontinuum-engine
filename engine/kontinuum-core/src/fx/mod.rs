//! Insert and bus effects.

mod chorus;
mod delay;
pub mod filter;
mod phaser;
mod reverb;
mod reverb2;
mod shifter;
mod saturate;
mod transient;

pub use chorus::Chorus;
pub use delay::Delay;
pub use filter::{FilterInsert, FilterMode, Svf};
pub use phaser::Phaser;
pub use reverb::Reverb;
pub use reverb2::ReverbV2;
pub use shifter::FreqShifter;
pub use saturate::Saturate;
pub use transient::TransientDesigner;

pub fn lp_coeff(sample_rate: f32, cutoff_hz: f32) -> f32 {
    1.0 - (-std::f32::consts::TAU * cutoff_hz.clamp(10.0, sample_rate * 0.45) / sample_rate).exp()
}
