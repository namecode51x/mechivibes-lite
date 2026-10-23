// Verifies the generated brown-noise asset with the app's own decoder.
//
// Run: cargo test --test brown_noise_asset -- --nocapture
//
// This is the check that matters for the user: the file has to actually decode
// with the same rodio/symphonia path the ambiance engine uses, and it has to
// loop without a click.

use rodio::{Decoder, Source};
use std::io::BufReader;

/// Decodes the whole asset through the app's decoder path.
fn decode_all(path: &std::path::Path) -> (Vec<i16>, u32, u16) {
    let file = std::fs::File::open(path).expect("asset must open");
    let mut decoder = Decoder::new(BufReader::new(file)).expect("asset must decode");
    let sample_rate = decoder.sample_rate();
    let channels = decoder.channels();
    let mut samples = Vec::new();
    for sample in decoder.by_ref() {
        samples.push(sample);
    }
    (samples, sample_rate, channels)
}

#[test]
fn brown_noise_asset_decodes_with_the_ambiance_decoder() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("sounds")
        .join("brown-noise.wav");
    assert!(path.is_file(), "the asset must exist: {}", path.display());

    let (samples, sample_rate, channels) = decode_all(&path);

    assert_eq!(sample_rate, 44100, "the asset is 44.1 kHz mono");
    assert_eq!(channels, 1, "the asset is mono");
    assert_eq!(samples.len(), 2_097_152, "the asset is 2^21 samples");
    assert!(
        samples.len() > 1_000_000,
        "the asset must be a long ambience bed ({} samples)",
        samples.len()
    );

    // The signal must not be silent, and must not be clipping.
    let peak = samples.iter().fold(0i16, |m, s| m.max(s.abs()));
    assert!(peak > 300, "the asset must not be silent (peak {peak})");
    assert!(peak < 32_767, "the asset must not clip (peak {peak})");

    // The seam: the last sample of the buffer and the first sample of the
    // buffer are adjacent when the engine loops the file, so the step across
    // that boundary must be no larger than a normal step inside the signal. A
    // bigger step is a click once per loop - the thing a looping ambience bed
    // must not do.
    let seam = (samples[samples.len() - 1] as i32 - samples[0] as i32).abs();
    let mut max_step = 0i32;
    for w in samples.windows(2) {
        max_step = max_step.max((w[1] as i32 - w[0] as i32).abs());
    }
    assert!(
        seam <= max_step,
        "loop seam step {seam} exceeds the signal's own largest step {max_step} - it would click"
    );

    // The spectrum must be brown: power falling 6 dB per octave. Measured on
    // the decoded samples, so this checks the shipped file and not the
    // generator's intermediate.
    let slope = measured_slope_per_octave(&samples, sample_rate);
    assert!(
        (4.5..=7.5).contains(&slope),
        "the asset's slope is {slope:.1} dB/octave; brown noise is 6"
    );

    println!(
        "brown-noise.wav: {sample_rate} Hz, {channels} ch, {} samples ({:.1}s), peak {peak}, \
         seam {seam} (max step {max_step}), slope {slope:.1} dB/octave",
        samples.len(),
        samples.len() as f64 / sample_rate as f64
    );
}

/// Power spectral slope in dB per octave, measured by comparing the energy
/// of two octave-wide bands (Goertzel-style bin sums over an FFT of a
/// decimated signal would be more code than this needs).
fn measured_slope_per_octave(samples: &[i16], sample_rate: u32) -> f64 {
    // Band energy via a simple DFT at a few frequencies, which is enough to
    // establish a slope: the 1/f^2 falloff shows up clearly across decades.
    let centre = |f: f64| -> f64 {
        // Goertzel: power at frequency f over the whole signal.
        let w = 2.0 * std::f64::consts::PI * f / sample_rate as f64;
        let coeff = 2.0 * w.cos();
        let (mut s0, mut s1, mut s2) = (0.0f64, 0.0f64, 0.0f64);
        for &x in samples.iter() {
            let x = x as f64 / 32768.0;
            s0 = x + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
        10.0 * power.log10()
    };

    let lo = centre(75.0);   // 50-100 Hz
    let hi = centre(1500.0); // 1000-2000 Hz
    let octaves = (1500.0f64 / 75.0f64).log2();
    (lo - hi) / octaves
}
