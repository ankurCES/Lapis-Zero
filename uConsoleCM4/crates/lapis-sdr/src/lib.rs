//! Lapis-Deck SDR runtime: IQ sources with a synthetic fallback, FFT spectrum and waterfall,
//! demodulators, decoder adapters and USB device detection.

pub mod decoders;
pub mod demod;
pub mod devices;
pub mod dsp;
pub mod iq;

pub use rustfft::num_complex::Complex32;
