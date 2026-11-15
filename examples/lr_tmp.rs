// Temporary: left/right ratio at the room's modes, per mic position. Not committed.
use correctsound::dsp::sweep::{Sweep, SweepParams};
use realfft::{RealFftPlanner, num_complex::Complex};
const RATE: f64 = 48000.0;
fn spectrum(path: &str, sweep: &Sweep) -> (Vec<Complex<f64>>, usize) {
    let rec = correctsound::session::read_wav(std::path::Path::new(path)).unwrap();
    let ir = sweep.deconvolve(&rec);
    let n = ir.len();
    let peak = ir.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
    let len = 1usize << 20;
    let mut buf = vec![0.0; len];
    // From the direct sound, 4 s, tapered at the end.
    let span = (4.0 * RATE) as usize;
    for i in 0..span {
        let w = ((span - i) as f64 / (0.5 * RATE)).min(1.0);
        buf[i] = ir[(peak + i) % n] * w;
    }
    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(len);
    let mut out = fft.make_output_vec();
    fft.process(&mut buf, &mut out).unwrap();
    (out, peak)
}
fn main() {
    let sweep = Sweep::new(SweepParams::default());
    let modes = [("x1 5.64", 30.4), ("y1 5.24", 32.7), ("x1y1", 44.9), ("x2 5.64", 60.8), ("z1 2.7", 63.5), ("y2", 65.5), ("x1z1", 70.2), ("y3", 97.6), ("x4", 121.2)];
    let bin = RATE / (1 << 20) as f64;
    for dir in std::env::args().skip(1) {
        for k in 1..=5 {
            let (l, r) = (format!("{dir}/left-{k}-capture.wav"), format!("{dir}/right-{k}-capture.wav"));
            if !std::path::Path::new(&l).exists() { continue; }
            let ((sl, pl), (sr, pr)) = (spectrum(&l, &sweep), spectrum(&r, &sweep));
            let mut row = format!("{} pos {k} (arrival L−R {:+.2} ms): ", dir.rsplit('/').next().unwrap(), (pl as f64 - pr as f64) / 48.0);
            for (name, f) in modes {
                // The mode's peak within ±0.6 Hz in the sum of both, then the ratio there.
                let k0 = ((f - 0.6) / bin) as usize; let k1 = ((f + 0.6) / bin) as usize;
                let kk = (k0..k1).max_by(|&a, &b| (sl[a].norm_sqr() + sr[a].norm_sqr()).total_cmp(&(sl[b].norm_sqr() + sr[b].norm_sqr()))).unwrap();
                let ratio = sl[kk] / sr[kk];
                row += &format!("{name} {:.1}: {:+.1} dB {:+4.0}° | ", kk as f64 * bin, 20.0 * ratio.norm().log10(), ratio.arg().to_degrees());
            }
            println!("{row}");
        }
    }
}
