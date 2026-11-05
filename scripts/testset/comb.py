"""Harmonic comb spacing over time: python3 -I comb.py AUDIO T0 T1 [fmax]"""
import subprocess, sys
import numpy as np
path, t0, t1 = sys.argv[1], float(sys.argv[2]), float(sys.argv[3])
fmax = float(sys.argv[4]) if len(sys.argv) > 4 else 400
sr = 4000
raw = subprocess.run(["ffmpeg", "-v", "error", "-ss", str(t0), "-to", str(t1), "-i", path, "-ac", "1", "-ar", str(sr),
                      "-f", "f32le", "-"], capture_output=True).stdout
x = np.frombuffer(raw, np.float32)
win = 2 * sr
hop = sr
nfft = 8 * sr  # 0.125 Hz bins
f = np.fft.rfftfreq(nfft, 1 / sr)
band = (f > 10) & (f < fmax)
for i in range(0, len(x) - win, hop):
    seg = x[i:i + win] * np.hanning(win)
    S = np.log(np.abs(np.fft.rfft(seg, nfft)) + 1e-9)[band]
    # whiten: subtract a 15 Hz running median-ish mean
    k = int(15 / (f[1] - f[0]))
    S = S - np.convolve(S, np.ones(k) / k, mode="same")
    S = np.maximum(S, 0)
    S -= S.mean()
    ac = np.correlate(S, S, "full")[len(S) - 1:]
    lags = np.arange(len(ac)) * (f[1] - f[0])
    m = (lags > 8) & (lags < 40)
    j = np.argmax(ac[m])
    sp = lags[m][j]
    strength = ac[m][j] / ac[0]
    print(f"{t0 + i / sr + 1:7.1f}s spacing {sp:5.2f} Hz  peak {strength:.2f}")
