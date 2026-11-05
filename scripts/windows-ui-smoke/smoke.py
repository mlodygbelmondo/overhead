#!/usr/bin/env python3
"""Window behaviour smoke test of overhead.exe on a real Windows desktop.

Moves the window by its title bar, double-clicks the title bar, resizes it from
edges and corners, uses the minimize / maximize / close buttons, drags a panel
splitter, snaps the window to the left screen edge, and checks that the window
place and the panel size are kept for the next start. Window rectangles before
and after each step go to results.json, screenshots to the output folder.

usage: smoke.py APP.exe OUT_DIR

Only the standard library and Pillow (screenshots) are needed. Real input:
SetCursorPos + mouse_event, as a user's mouse would send it.
"""
import ctypes
import ctypes.wintypes as W
import json
import math
import os
import random
import struct
import subprocess
import sys
import time
import wave

app, out = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)

try:
    ctypes.windll.shcore.SetProcessDpiAwareness(2)  # per-monitor: physical pixels everywhere
except Exception:
    ctypes.windll.user32.SetProcessDPIAware()

user32 = ctypes.windll.user32
dwm = ctypes.windll.dwmapi
LEFTDOWN, LEFTUP = 0x0002, 0x0004
SW_RESTORE = 9

try:
    from PIL import ImageGrab
except ImportError:
    ImageGrab = None

results = {"steps": [], "screen": None}
failures = []


def log(*a):
    print(*a, flush=True)


def shot(name):
    if ImageGrab is None:
        return
    try:
        ImageGrab.grab(all_screens=True).save(os.path.join(out, f"{name}.png"))
    except Exception as e:  # a locked or missing desktop
        log(f"screenshot {name} failed: {e}")


def rect(h):
    r = W.RECT()
    user32.GetWindowRect(h, ctypes.byref(r))
    return [r.left, r.top, r.right, r.bottom]


def visible_rect(h):
    """The window as drawn (without the invisible resize borders)."""
    r = W.RECT()
    if dwm.DwmGetWindowAttribute(h, 9, ctypes.byref(r), ctypes.sizeof(r)) == 0:
        return [r.left, r.top, r.right, r.bottom]
    return rect(h)


def client_rect(h):
    r = W.RECT()
    user32.GetClientRect(h, ctypes.byref(r))
    p = W.POINT(0, 0)
    user32.ClientToScreen(h, ctypes.byref(p))
    return [p.x, p.y, p.x + r.right, p.y + r.bottom]


def zoomed(h):
    return bool(user32.IsZoomed(h))


def iconic(h):
    return bool(user32.IsIconic(h))


def scale(h):
    try:
        return user32.GetDpiForWindow(h) / 96.0
    except Exception:
        return 1.0


def work_area():
    r = W.RECT()
    user32.SystemParametersInfoW(0x0030, 0, ctypes.byref(r), 0)  # SPI_GETWORKAREA
    return [r.left, r.top, r.right, r.bottom]


def move(x, y):
    user32.SetCursorPos(int(x), int(y))


def drag(x0, y0, x1, y1, steps=24, hold=0.25):
    move(x0, y0)
    time.sleep(0.25)
    user32.mouse_event(LEFTDOWN, 0, 0, 0, 0)
    time.sleep(hold)
    for i in range(1, steps + 1):
        f = i / steps
        move(x0 + (x1 - x0) * f, y0 + (y1 - y0) * f)
        time.sleep(0.02)
    time.sleep(0.25)
    user32.mouse_event(LEFTUP, 0, 0, 0, 0)
    time.sleep(0.8)


def click(x, y, n=1):
    move(x, y)
    time.sleep(0.3)
    for _ in range(n):
        user32.mouse_event(LEFTDOWN, 0, 0, 0, 0)
        time.sleep(0.05)
        user32.mouse_event(LEFTUP, 0, 0, 0, 0)
        time.sleep(0.08)
    time.sleep(1.2)


def near(a, b, tol=4):
    return all(abs(x - y) <= tol for x, y in zip(a, b))


def step(name, ok, **info):
    info = {"step": name, "ok": bool(ok), **info}
    results["steps"].append(info)
    log(("PASS " if ok else "FAIL ") + name + "  " + json.dumps({k: v for k, v in info.items() if k not in ("step", "ok")}))
    if not ok:
        failures.append(name)


def find_window(pid, timeout=90):
    found = []

    @ctypes.WINFUNCTYPE(W.BOOL, W.HWND, W.LPARAM)
    def cb(h, _):
        p = W.DWORD()
        user32.GetWindowThreadProcessId(h, ctypes.byref(p))
        if p.value == pid and user32.IsWindowVisible(h):
            n = user32.GetWindowTextLengthW(h)
            buf = ctypes.create_unicode_buffer(n + 1)
            user32.GetWindowTextW(h, buf, n + 1)
            if "Overhead" in buf.value:
                found.append(h)
        return True

    t0 = time.time()
    while time.time() - t0 < timeout:
        found.clear()
        user32.EnumWindows(cb, 0)
        if found:
            return found[0]
        time.sleep(0.5)
    return None


def synth_wav(path, seconds=60, sr=16000):
    """One propeller aircraft passing at 30 s (Doppler-shifted harmonics) in noise."""
    c, v, d, tc, f0 = 343.0, 60.0, 400.0, 30.0, 110.0
    rnd = random.Random(1)
    phase = [0.0] * 6
    frames = bytearray()
    for n in range(seconds * sr):
        t = n / sr
        x = v * (t - tc)
        r = math.hypot(x, d)
        vr = v * x / r  # receding speed
        f = f0 * c / (c + vr)
        amp = 0.25 * min(1.0, 600.0 / r)
        s = 0.0
        for k in range(6):
            phase[k] += 2 * math.pi * f * (k + 1) / sr
            s += amp / (k + 1) * math.sin(phase[k])
        s += rnd.gauss(0, 0.05)
        frames += struct.pack("<h", max(-32767, min(32767, int(s * 32767))))
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(bytes(frames))


def launch(args, logname):
    env = dict(os.environ)
    env["OVERHEAD_LOG"] = "info"
    logf = open(os.path.join(out, logname), "w")
    p = subprocess.Popen([app] + args, stdout=logf, stderr=subprocess.STDOUT, env=env)
    h = find_window(p.pid)
    return p, h


wav = os.path.join(out, "smoke.wav")
synth_wav(wav)
results["screen"] = {"w": user32.GetSystemMetrics(0), "h": user32.GetSystemMetrics(1), "work_area": work_area()}
log("screen", results["screen"])

appdata = os.environ.get("APPDATA", "")
layout_file = os.path.join(appdata, "Overhead", "layout.json")
if os.path.exists(layout_file):
    os.remove(layout_file)

p, h = launch([wav, "--default-layout", "--size", "1200x760"], "app-1.log")
step("window opens", h is not None)
if h is None:
    shot("00-no-window")
    p.kill()
    json.dump(results, open(os.path.join(out, "results.json"), "w"), indent=1)
    sys.exit(1)
time.sleep(10)  # decode + detection of the 1-minute file
s = scale(h)
r0 = rect(h)
results["scale"] = s
results["first"] = {"window": r0, "visible": visible_rect(h), "client": client_rect(h)}
log("window", results["first"])
shot("01-start")

# 1. drag the title bar (an empty part between the file name and the buttons)
cl = client_rect(h)
tx, ty = cl[0] + int(460 * s), cl[1] + int(18 * s)
drag(tx, ty, tx + 140, ty + 90)
r1 = rect(h)
step("title bar drag moves the window", near(r1, [r0[0] + 140, r0[1] + 90, r0[2] + 140, r0[3] + 90]), before=r0, after=r1)
shot("02-moved")

# 2. double click on the title bar: maximize, again: restore
cl = client_rect(h)
tx, ty = cl[0] + int(460 * s), cl[1] + int(18 * s)
click(tx, ty, 2)
r2 = rect(h)
step("title bar double click maximizes", zoomed(h), before=r1, after=r2, work_area=work_area())
shot("03-maximized-dblclick")
cl = client_rect(h)
click(cl[0] + int(460 * s), cl[1] + int(18 * s), 2)
r3 = rect(h)
step("second double click restores", not zoomed(h) and near(r3, r1), before=r2, after=r3)

# 3. resize from the right edge, the bottom-right corner, the left edge, the top edge
r = rect(h)
vis = visible_rect(h)
ex = r[2] - max(2, (r[2] - vis[2]) // 2) if r[2] > vis[2] + 2 else r[2] - 2
my = (r[1] + r[3]) // 2
drag(ex, my, ex + 150, my)
ra = rect(h)
step("right edge resize", abs((ra[2] - ra[0]) - (r[2] - r[0]) - 150) <= 4 and abs(ra[0] - r[0]) <= 2, before=r, after=ra)

r = rect(h)
vis = visible_rect(h)
cx_ = r[2] - max(2, (r[2] - vis[2]) // 2) if r[2] > vis[2] + 2 else r[2] - 2
cy_ = r[3] - max(2, (r[3] - vis[3]) // 2) if r[3] > vis[3] + 2 else r[3] - 2
drag(cx_, cy_, cx_ - 100, cy_ - 60)
rb = rect(h)
step(
    "bottom-right corner resize",
    abs((rb[2] - rb[0]) - (r[2] - r[0]) + 100) <= 4 and abs((rb[3] - rb[1]) - (r[3] - r[1]) + 60) <= 4,
    before=r,
    after=rb,
)

r = rect(h)
vis = visible_rect(h)
lx = r[0] + max(2, (vis[0] - r[0]) // 2) if vis[0] > r[0] + 2 else r[0] + 2
my = (r[1] + r[3]) // 2
drag(lx, my, lx - 90, my)
rc = rect(h)
step("left edge resize", abs(rc[0] - (r[0] - 90)) <= 4 and abs(rc[2] - r[2]) <= 2, before=r, after=rc)

r = rect(h)
cl = client_rect(h)
mx = (cl[0] + cl[2]) // 2
drag(mx, cl[1] + 1, mx, cl[1] + 1 - 50)
rd = rect(h)
step("top edge resize", abs(rd[1] - (r[1] - 50)) <= 4 and abs(rd[3] - r[3]) <= 2, before=r, after=rd)
shot("04-resized")

# 4. the caption buttons (44 logical px each, at the right end of the title bar)
cl = client_rect(h)
by = cl[1] + int(18 * s)
max_x = cl[2] - int(66 * s)
before = rect(h)
click(max_x, by)
rm = rect(h)
step("maximize button", zoomed(h), before=before, after=rm)
shot("05-maximized-button")
cl = client_rect(h)
click(cl[2] - int(66 * s), cl[1] + int(18 * s))
rr = rect(h)
step("restore button", not zoomed(h) and near(rr, before), before=rm, after=rr)

cl = client_rect(h)
click(cl[2] - int(110 * s), cl[1] + int(18 * s))
step("minimize button", iconic(h), after=rect(h))
user32.ShowWindow(h, SW_RESTORE)
time.sleep(1.5)
step("restored after minimize", not iconic(h) and near(rect(h), rr), after=rect(h))

# 5. a splitter: the line between the input form and the centre (form 292 px wide by default)
cl = client_rect(h)
sx = cl[0] + int(292 * s)
sy = cl[1] + int(300 * s)
drag(sx, sy, sx + int(60 * s), sy)
shot("06-splitter")
splitter_moved = {"from_x": sx, "to_x": sx + int(60 * s)}

# 5b. controls inside the title bar (the rest of it is a native drag area):
# a click must reach the switch and must not move the window. The app logs
# each switch, so the log tells whether the click arrived.
def app_log(name):
    try:
        return open(os.path.join(out, name), encoding="utf-8", errors="replace").read()
    except OSError:
        return ""


before_sw = rect(h)
cl = client_rect(h)
# "Spectrogram" switch: ~357 logical px left of the window's right edge
click(cl[2] - int(357 * s), cl[1] + int(18 * s))
time.sleep(1.0)
spectro_lines = [l for l in app_log("app-1.log").splitlines() if "spectrogram shown:" in l]
step("title bar switch (Spectrogram) clicks", len(spectro_lines) == 1 and near(rect(h), before_sw), log=spectro_lines, before=before_sw, after=rect(h))
shot("06b-spectrogram-switch")
# the form toggle at the left end of the title bar
cl = client_rect(h)
click(cl[0] + int(20 * s), cl[1] + int(18 * s))
time.sleep(1.0)
form_lines = [l for l in app_log("app-1.log").splitlines() if "form shown:" in l]
step("title bar button (form) clicks", len(form_lines) == 1 and "false" in form_lines[0] and near(rect(h), before_sw), log=form_lines, after=rect(h))
shot("06c-form-folded")
# and open again
cl = client_rect(h)
click(cl[0] + int(20 * s), cl[1] + int(18 * s))
time.sleep(1.0)
form_lines = [l for l in app_log("app-1.log").splitlines() if "form shown:" in l]
step("title bar button (form) clicks again", len(form_lines) == 2 and "true" in form_lines[1], log=form_lines)

# 6. Aero snap: drag the title bar to the left edge of the screen
before_snap = rect(h)
cl = client_rect(h)
tx, ty = cl[0] + int(460 * s), cl[1] + int(18 * s)
wa = work_area()
drag(tx, ty, wa[0], ty, steps=40, hold=0.3)
time.sleep(1.0)
rs = visible_rect(h)
# the left half, unless the smallest window size (940 logical px) is wider
half = max((wa[2] - wa[0]) // 2, int(940 * s))
snapped = abs(rs[0] - wa[0]) <= 12 and abs((rs[2] - rs[0]) - half) <= 24 and abs(rs[1] - wa[1]) <= 12 and abs(rs[3] - wa[3]) <= 12
step("aero snap to the left half (drag to the screen edge)", snapped, before=before_snap, after_visible=rs, work_area=wa)
shot("07-snapped-left")
# the escape key closes a Snap Assist overlay if one opened
user32.keybd_event(0x1B, 0, 0, 0)
user32.keybd_event(0x1B, 0, 2, 0)
time.sleep(0.5)
# drag it back off the edge (restores the size it had before the snap)
cl = client_rect(h)
drag(cl[0] + int(200 * s), cl[1] + int(18 * s), cl[0] + int(200 * s) + 300, cl[1] + int(18 * s) + 120, steps=30)
time.sleep(1.0)
last = rect(h)
results["before_close"] = {"window": last, "zoomed": zoomed(h)}
shot("08-unsnapped")

# 7. the close button ends the app and keeps the layout
cl = client_rect(h)
click(cl[2] - int(22 * s), cl[1] + int(18 * s))
try:
    p.wait(timeout=10)
    exited = True
except subprocess.TimeoutExpired:
    exited = False
step("close button ends the app", exited, exit_code=p.returncode)
saved = None
if os.path.exists(layout_file):
    saved = json.load(open(layout_file))
results["layout_json"] = saved
step("layout.json written", saved is not None, path=layout_file)
if saved:
    step("splitter drag kept (form wider)", saved.get("sidebar_w", 0) >= 292 + 40, sidebar_w=saved.get("sidebar_w"), drag=splitter_moved)
if not exited:
    p.kill()

# 8. a new start opens where the window was
p2, h2 = launch([wav], "app-2.log")
step("second start opens a window", h2 is not None)
if h2:
    time.sleep(6)
    r2nd = rect(h2)
    step("window place kept between runs", near(r2nd, last, tol=6), before_close=last, reopened=r2nd)
    shot("09-reopened")
    # maximized when closed: maximized at the next start, restoring to the old place
    cl = client_rect(h2)
    click(cl[2] - int(66 * s), cl[1] + int(18 * s))
    step("maximize before closing", zoomed(h2), after=rect(h2))
    user32.PostMessageW(h2, 0x0010, 0, 0)  # WM_CLOSE
    try:
        p2.wait(timeout=10)
    except subprocess.TimeoutExpired:
        p2.kill()
    p3, h3 = launch([wav], "app-3.log")
    step("third start opens a window", h3 is not None)
    if h3:
        time.sleep(6)
        step("maximized state kept between runs", zoomed(h3), after=rect(h3))
        shot("10-reopened-maximized")
        cl = client_rect(h3)
        click(cl[2] - int(66 * s), cl[1] + int(18 * s))
        step("restores to the place it had before", not zoomed(h3) and near(rect(h3), r2nd, tol=6), before=r2nd, after=rect(h3))
        user32.PostMessageW(h3, 0x0010, 0, 0)
        try:
            p3.wait(timeout=10)
        except subprocess.TimeoutExpired:
            p3.kill()

results["failures"] = failures
json.dump(results, open(os.path.join(out, "results.json"), "w"), indent=1)
log("failures:", failures or "none")
sys.exit(1 if failures else 0)
