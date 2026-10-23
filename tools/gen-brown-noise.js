// Generates the brown-noise ambience bed used by the Mood page.
//
// Brown noise is white noise with a 1/f^2 power spectrum - the ear reads it
// as a deep, warm rumble rather than the hiss of white or the "frying" edge of
// pink. It is synthesised here instead of downloaded so the asset is
// licence-clean and can be tuned to the app's own mix.
//
// Two properties matter for how this file is used:
//
//   - The right slope. Built in the frequency domain (scale every FFT bin by
//     1/f, inverse-transform), because a time-domain leaky integrator's corner
//     frequency depends on its leak coefficient: pick it too high and the
//     output is white above that point, too low and the walk wanders while the
//     DC blocker fights it. Scaling the bins gives -6 dB/octave directly.
//
//   - A seamless loop. The ambiance engine plays these with
//     `repeat_infinite()`, so the buffer must wrap onto itself without a step
//     at the seam. An inverse FFT of a spectrum is inherently periodic, so the
//     loop point is continuous by construction.
//
// The bin gain is normalised so the largest gain is 1: scaling raw bins by
// 1/k gives the lowest bins gains of tens of thousands, and the inverse
// transform's sum is then dominated by them, so double-precision cancellation
// erases the high frequencies.
//
// Output: 16-bit mono WAV at 44.1 kHz. WAV rather than MP3 because it has no
// encoder delay and no padding, so the loop point stays exact; the ambience
// sounds are looped indefinitely and any gap at the seam would click.

const SAMPLE_RATE = 44100;
const N = 1 << 21;              // 2,097,152 samples = 47.6 s
const F_LOW = 30;               // Hz: roll-off below this (below hearing)
const SLOPE = 2;                // PSD ~ 1/f^2 -> brown noise, -6 dB/octave
const PEAK_DBFS = -3;           // headroom; the app applies its own per-sound volume

// Self-test: the FFT this file depends on is verified before it is used, so a
// broken transform shows up here rather than as a subtly wrong asset. A pure
// sine must land in exactly one bin, and a round trip must reproduce the
// input.
function selfTestFft(fft) {
  const n = 1024;
  const re = new Float64Array(n), im = new Float64Array(n);
  const bin = 100;
  for (let i = 0; i < n; i++) re[i] = Math.cos((2 * Math.PI * bin * i) / n);
  fft(re, im, false);
  let peak = 0, peakBin = -1;
  for (let k = 0; k < n / 2; k++) {
    const mag = Math.hypot(re[k], im[k]);
    if (mag > peak) { peak = mag; peakBin = k; }
  }
  if (peakBin !== bin) {
    throw new Error(`FFT self-test failed: sine landed in bin ${peakBin}, expected ${bin}`);
  }

  const rt = new Float64Array(n), it = new Float64Array(n);
  for (let i = 0; i < n; i++) rt[i] = Math.sin(i * 0.37) + 0.5 * Math.cos(i * 0.11);
  const original = Float64Array.from(rt);
  fft(rt, it, false);
  fft(rt, it, true);
  let err = 0;
  for (let i = 0; i < n; i++) err = Math.max(err, Math.abs(rt[i] - original[i]));
  if (err > 1e-9) {
    throw new Error(`FFT self-test failed: round-trip error ${err}`);
  }
  return { peakBin, err };
}

function makeFft(n) {
  return function fft(re, im, inverse) {
    for (let i = 1, j = 0; i < n; i++) {
      let bit = n >> 1;
      for (; j & bit; bit >>= 1) j ^= bit;
      j ^= bit;
      if (i < j) {
        let t = re[i]; re[i] = re[j]; re[j] = t;
        t = im[i]; im[i] = im[j]; im[j] = t;
      }
    }
    for (let len = 2; len <= n; len <<= 1) {
      const ang = (2 * Math.PI / len) * (inverse ? 1 : -1);
      const wRe = Math.cos(ang), wIm = Math.sin(ang);
      for (let i = 0; i < n; i += len) {
        let curRe = 1, curIm = 0;
        const half = len >> 1;
        for (let k = 0; k < half; k++) {
          const aRe = re[i + k], aIm = im[i + k];
          const bRe = re[i + k + half] * curRe - im[i + k + half] * curIm;
          const bIm = re[i + k + half] * curIm + im[i + k + half] * curRe;
          re[i + k] = aRe + bRe;       im[i + k] = aIm + bIm;
          re[i + k + half] = aRe - bRe; im[i + k + half] = aIm - bIm;
          const nRe = curRe * wRe - curIm * wIm;
          curIm = curRe * wIm + curIm * wRe;
          curRe = nRe;
        }
      }
    }
    if (inverse) {
      for (let i = 0; i < n; i++) { re[i] /= n; im[i] /= n; }
    }
  };
}

// Deterministic PRNG (xorshift32) so the asset is reproducible from source.
let rngState = 0x1a2b3c4d | 0;
function nextRandom() {
  rngState ^= rngState << 13; rngState >>>= 0;
  rngState ^= rngState >> 17;
  rngState ^= rngState << 5;  rngState >>>= 0;
  return rngState / 0x100000000;
}

function generate() {
  const fft = makeFft(N);
  const test = selfTestFft(makeFft(1024));
  console.log(`FFT self-test OK (sine bin ${test.peakBin}, round-trip err ${test.err.toExponential(1)})`);

  const re = new Float64Array(N), im = new Float64Array(N);
  for (let i = 0; i < N; i++) re[i] = nextRandom() * 2 - 1;
  fft(re, im, false);

  const binHz = SAMPLE_RATE / N;
  const half = SLOPE / 2;
  for (let k = 1; k < N / 2; k++) {
    const f = k * binHz;
    const g = f <= F_LOW
      ? Math.pow(f / F_LOW, half)          // smooth high-pass below the roll-off
      : Math.pow(F_LOW / f, half);
    re[k] *= g; im[k] *= g;
    re[N - k] *= g; im[N - k] *= g;
  }
  re[0] = 0; im[0] = 0;
  if (N % 2 === 0) { re[N / 2] = 0; im[N / 2] = 0; }

  fft(re, im, true);
  const out = new Float32Array(N);
  for (let i = 0; i < N; i++) out[i] = re[i];
  return out;
}

const pcm = generate();

// Normalise to the target peak.
let peak = 0;
for (let i = 0; i < pcm.length; i++) {
  const a = Math.abs(pcm[i]);
  if (a > peak) peak = a;
}
const scale = peak > 0 ? Math.pow(10, PEAK_DBFS / 20) / peak : 1;
for (let i = 0; i < pcm.length; i++) pcm[i] *= scale;

// --- Write 16-bit PCM WAV --------------------------------------------------
function writeWav(path, samples) {
  const dataBytes = samples.length * 2;
  const buf = Buffer.alloc(44 + dataBytes);
  buf.write('RIFF', 0);
  buf.writeUInt32LE(36 + dataBytes, 4);
  buf.write('WAVE', 8);
  buf.write('fmt ', 12);
  buf.writeUInt32LE(16, 16);          // PCM chunk size
  buf.writeUInt16LE(1, 20);           // format = PCM
  buf.writeUInt16LE(1, 22);           // channels = mono
  buf.writeUInt32LE(SAMPLE_RATE, 24);
  buf.writeUInt32LE(SAMPLE_RATE * 2, 28); // byte rate
  buf.writeUInt16LE(2, 32);           // block align
  buf.writeUInt16LE(16, 34);          // bits per sample
  buf.write('data', 36);
  buf.writeUInt32LE(dataBytes, 40);
  for (let i = 0; i < samples.length; i++) {
    const s = Math.max(-1, Math.min(1, samples[i]));
    buf.writeInt16LE(Math.round(s * 32767), 44 + i * 2);
  }
  require('fs').writeFileSync(path, buf);
}

writeWav(process.argv[2] || 'brown-noise.wav', pcm);

// --- Verify the result -----------------------------------------------------
{
  const fft = makeFft(N);
  const re = Float64Array.from(pcm), im = new Float64Array(N);
  fft(re, im, false);
  const binHz = SAMPLE_RATE / N;
  const bandDb = (loHz, hiHz) => {
    const lo = Math.max(1, Math.round(loHz / binHz));
    const hi = Math.min(N / 2, Math.round(hiHz / binHz));
    let sum = 0;
    for (let k = lo; k < hi; k++) sum += re[k] * re[k] + im[k] * im[k];
    return 10 * Math.log10(sum / Math.max(1, hi - lo));
  };
  const a = bandDb(50, 100), b = bandDb(1000, 2000), c = bandDb(8000, 16000);
  console.log(`frames=${pcm.length} (${(pcm.length / SAMPLE_RATE).toFixed(1)}s) @ ${SAMPLE_RATE} Hz mono`);
  console.log(`50-100 Hz=${a.toFixed(1)} dB  1k-2k=${b.toFixed(1)} dB  8k-16k=${c.toFixed(1)} dB`);
  console.log(`slope: ${((a - b) / Math.log2(1500 / 75)).toFixed(1)} / ${((b - c) / Math.log2(12000 / 1500)).toFixed(1)} dB per octave (target 6.0)`);

  // The seam step is the discontinuity the loop would click on. It is
  // compared against the largest step anywhere inside the buffer: a seam
  // that is no bigger than the signal's own normal sample-to-sample
  // movement is inaudible.
  let rms = 0;
  for (let i = 0; i < pcm.length; i++) rms += pcm[i] * pcm[i];
  rms = Math.sqrt(rms / pcm.length);
  let maxStep = 0;
  for (let i = 1; i < pcm.length; i++) {
    const d = Math.abs(pcm[i] - pcm[i - 1]);
    if (d > maxStep) maxStep = d;
  }
  const seam = Math.abs(pcm[0] - pcm[pcm.length - 1]);
  console.log(`rms=${rms.toFixed(5)}  max step inside=${maxStep.toFixed(5)}  seam step=${seam.toFixed(5)}`);

  // Fail loudly rather than shipping a wrong asset: a brown-noise bed that is
  // actually white, or that clicks once per loop, is worse than no file.
  const slope = ((a - b) / Math.log2(1500 / 75));
  if (Math.abs(slope - 6) > 1.0) {
    throw new Error(`spectral slope is ${slope.toFixed(1)} dB/octave, expected ~6 (brown noise)`);
  }
  if (seam > maxStep) {
    throw new Error(`loop seam step ${seam.toFixed(5)} exceeds the signal's largest internal step ${maxStep.toFixed(5)} - it would click`);
  }
  console.log(`seam within normal range: YES (verified)`);
}
