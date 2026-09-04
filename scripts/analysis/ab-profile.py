import wave, numpy as np, sys

def load(path):
    w = wave.open(path)
    sr = w.getframerate(); n = w.getnframes(); ch = w.getnchannels()
    data = np.frombuffer(w.readframes(n), dtype=np.int16).astype(np.float64) / 32768.0
    data = data.reshape(-1, ch)
    return sr, data

BANDS = [("sub 20-60", 20, 60), ("bass 60-150", 60, 150), ("lowmid 150-400", 150, 400),
         ("mid 400-2k", 400, 2000), ("himid 2k-6k", 2000, 6000), ("high 6k-16k", 6000, 16000)]

def analyze(path, name):
    sr, d = load(path)
    mono = d.mean(axis=1)
    L, R = d[:, 0], d[:, 1]
    rms = np.sqrt((mono**2).mean()); peak = np.abs(d).max()
    crest = 20*np.log10(peak/max(rms, 1e-12))
    # windowed spectra
    W = 8192; hop = 4096
    nwin = (len(mono)-W)//hop
    win = np.hanning(W)
    freqs = np.fft.rfftfreq(W, 1/sr)
    spec = np.zeros(len(freqs)); side_spec = np.zeros(len(freqs)); mid_spec = np.zeros(len(freqs))
    # Accumulated separately, per frame, to match the Rust port exactly:
    # sum(|X|) is not sqrt(sum(|X|²)).
    mag_spec = np.zeros(len(freqs))
    mid_c, side_c = (L+R)/2, (L-R)/2
    for i in range(nwin):
        s = slice(i*hop, i*hop+W)
        X = np.abs(np.fft.rfft(mono[s]*win))
        spec += X**2
        mag_spec += X
        mid_spec += np.abs(np.fft.rfft(mid_c[s]*win))**2
        side_spec += np.abs(np.fft.rfft(side_c[s]*win))**2
    total = spec.sum()
    bands = {n2: spec[(freqs>=lo)&(freqs<hi)].sum()/total for n2, lo, hi in BANDS}
    # Band shares are energy shares, so power-weighted. The centroid follows the
    # usual magnitude convention: weighting it by power squares the low end's
    # advantage and pins the figure in the low hundreds of Hz for any bass-heavy
    # master, whatever its actual brightness.
    centroid = (freqs*mag_spec).sum()/mag_spec.sum()
    corr = np.corrcoef(L, R)[0,1]
    width = side_spec.sum()/max(mid_spec.sum(),1e-12)
    hf = (freqs>=2000)
    width_hi = side_spec[hf].sum()/max(mid_spec[hf].sum(),1e-12)
    # Short-term dynamics: 5th-to-95th percentile spread of the 400 ms RMS
    # windows. Max-over-min is a single-outlier statistic — the quietest window
    # is nearly always window 0, the lead-in before the first hit — so it
    # described the fade-in rather than the arrangement.
    w400 = int(0.4*sr)
    st = np.array([np.sqrt((mono[i:i+w400]**2).mean()) for i in range(0, len(mono)-w400, w400)])
    st_db = 20*np.log10(np.maximum(st, 1e-12))
    dyn = np.percentile(st_db, 95) - np.percentile(st_db, 5)
    # transient analysis via spectral flux on HF
    Wf, hf_hop = 1024, 512
    nf = (len(mono)-Wf)//hf_hop
    fw = np.hanning(Wf); prev = None; flux = []
    fr = np.fft.rfftfreq(Wf, 1/sr); hfm = fr>3000
    hi_mag_sum = 0.0; all_mag_sum = 0.0; max_mag = 0.0
    for i in range(nf):
        full = np.abs(np.fft.rfft(mono[i*hf_hop:i*hf_hop+Wf]*fw))
        m = full[hfm]
        all_mag_sum += full.sum(); hi_mag_sum += m.sum()
        max_mag = max(max_mag, m.max() if m.size else 0.0)
        if prev is not None: flux.append(np.maximum(m-prev,0).sum())
        prev = m
    flux = np.array(flux)
    # A band with nothing in it has no transients in it: below this share
    # everything above 3 kHz is leakage and quantization noise, whose churn is
    # pure flux. The noise floor is also taken within the measured band — using
    # the full-spectrum max let the kick set the threshold for hat detection.
    if len(flux) < 3 or hi_mag_sum <= all_mag_sum * 5e-3:
        peaks = []
    else:
        # A hit must clear the median by a wide margin; mean+1.5σ alone marks
        # the top few percent of any stationary wobble as "hits".
        th = max(flux.mean()+1.5*flux.std(), max_mag*1e-3, np.median(flux)*3.0)
        peaks = [flux[i] for i in range(1,len(flux)-1) if flux[i]>th and flux[i]>=flux[i-1] and flux[i]>=flux[i+1]]
    if len(peaks) <= 3: peaks = []
    hitvar = float(np.std(peaks)/np.mean(peaks)) if len(peaks)>3 else 0.0
    print(f"== {name}")
    print(f"  rms {20*np.log10(rms):6.1f} dBFS   peak {20*np.log10(peak):5.1f} dBFS   crest {crest:4.1f} dB   short-term dyn (P95-P5) {dyn:4.1f} dB")
    print(f"  centroid {centroid:6.0f} Hz   L/R corr {corr:5.2f}   width(S/M) {width:5.3f}   width>2k {width_hi:5.3f}")
    print(f"  bands: " + "  ".join(f"{k}={v*100:4.1f}%" for k,v in bands.items()))
    print(f"  transients/sec {len(peaks)/ (len(mono)/sr):4.1f}   per-hit variation (cv of hit strength) {hitvar:4.2f}")

analyze(sys.argv[1], sys.argv[2]); analyze(sys.argv[3], sys.argv[4])
