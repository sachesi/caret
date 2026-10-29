#!/usr/bin/env python3
"""Measures Caret in a headless Sway session of its own: how fast output goes through,
how long frames take, how soon a typed key comes back, how often an idle window wakes
up and how much memory it holds.

    bench/bench.py [--caret target/release/caret] [--out target/bench] [--runs 3]

Needs sway, swaymsg and wdotool. Every run gets a session bus of its own and settings in
memory, so nothing of the user's is read or changed. The summary goes to stdout and,
with the raw timings, under --out.
"""

import argparse
import json
import os
import random
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
APP_ID = "io.github.sachesi.caret"
RESOLUTION = "1920x1080"


def generate(data: Path) -> dict[str, Path]:
    """The workloads, written once: plain lines, colourful full-screen redraws, and text
    beyond ASCII. The same bytes every time, from a fixed seed."""
    data.mkdir(parents=True, exist_ok=True)
    files = {
        "plain": data / "plain.txt",
        "redraw": data / "redraw.txt",
        "unicode": data / "unicode.txt",
    }
    if all(path.exists() for path in files.values()):
        return files
    rng = random.Random(7)
    with files["plain"].open("w") as out:
        for line in range(2_000_000):
            out.write(f"{line:>9} the quick brown fox jumps over the lazy dog\n")
    with files["redraw"].open("w") as out:
        letters = "abcdefghijklmnopqrstuvwxyz0123456789{}[]()<>=-+*/"
        for _ in range(1500):
            out.write("\x1b[H")
            for _ in range(70):
                for _ in range(30):
                    out.write(f"\x1b[38;5;{rng.randrange(256)}m")
                    out.write("".join(rng.choice(letters) for _ in range(8)))
                out.write("\x1b[0m\r\n")
    with files["unicode"].open("w") as out:
        samples = [
            "日本語のテキストと漢字",
            "Ελληνικά και Кириллица",
            "é ä ô combining",
            "─│┌┐└┘├┤┬┴┼ ▀▄█▌▐░▒▓",
            "😀 🚀 🐧 ✅ ❌",
            "->  =>  !=  ==  <=  >=  ::  ...",
        ]
        for line in range(200_000):
            out.write(f"{line:>7} {samples[line % len(samples)]} {samples[(line * 7) % len(samples)]}\n")
    return files


class Session:
    """A headless Sway with a virtual keyboard, in a runtime directory of its own."""

    def __init__(self, state: Path):
        self.state = state
        self.runtime = state / "runtime"
        self.runtime.mkdir(parents=True, exist_ok=True)
        os.chmod(self.runtime, 0o700)
        config = state / "sway.conf"
        config.write_text(
            f"output HEADLESS-1 resolution {RESOLUTION} position 0 0\n"
            "default_border none\nfocus_follows_mouse no\nxwayland disable\n"
        )
        self.env = {
            key: value
            for key, value in os.environ.items()
            if key not in ("DISPLAY", "WAYLAND_DISPLAY", "SWAYSOCK", "DBUS_SESSION_BUS_ADDRESS")
        }
        self.env |= {
            "XDG_RUNTIME_DIR": str(self.runtime),
            "SWAYSOCK": str(self.runtime / "sway.sock"),
            "XDG_SESSION_TYPE": "wayland",
            "GDK_BACKEND": "wayland",
        }
        sway_env = self.env | {"WLR_BACKENDS": "headless", "WLR_HEADLESS_OUTPUTS": "1"}
        node = next(
            (str(path) for path in sorted(Path("/dev/dri").glob("renderD*")) if os.access(path, os.R_OK | os.W_OK)),
            None,
        )
        sway_env["WLR_RENDERER"] = "gles2" if node else "pixman"
        if node:
            sway_env["WLR_RENDER_DRM_DEVICE"] = node
        self.sway = subprocess.Popen(
            ["sway", "-c", str(config)],
            env=sway_env,
            stdout=(state / "sway.log").open("w"),
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        self.wait(lambda: any(self.runtime.glob("wayland-*")) and self.swaymsg("-t", "get_version"), "sway")
        socket = next(path.name for path in self.runtime.glob("wayland-*") if not path.name.endswith(".lock"))
        self.env["WAYLAND_DISPLAY"] = socket
        self.prime = subprocess.Popen(
            ["wdotool", "prime"],
            env=self.env,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            start_new_session=True,
        )
        if self.prime.stdout is None or not self.prime.stdout.readline().startswith("ready"):
            self.close()
            sys.exit("wdotool prime did not come up")

    def swaymsg(self, *args: str) -> bool:
        return subprocess.run(["swaymsg", *args], env=self.env, capture_output=True).returncode == 0

    def wait(self, ready, what: str, timeout: float = 20.0) -> None:
        deadline = time.monotonic() + timeout
        while not ready():
            if time.monotonic() > deadline:
                self.close()
                sys.exit(f"{what} did not come up within {timeout:.0f} s; see {self.state}")
            time.sleep(0.05)

    def windows(self) -> int:
        tree = subprocess.run(["swaymsg", "-t", "get_tree", "-r"], env=self.env, capture_output=True, text=True)
        if tree.returncode != 0:
            return 0
        count = 0
        stack = [json.loads(tree.stdout)]
        while stack:
            node = stack.pop()
            count += node.get("type") == "con" and node.get("pid") is not None
            stack += node.get("nodes", []) + node.get("floating_nodes", [])
        return count

    def wdotool(self, *args: str) -> None:
        subprocess.run(
            ["wdotool", "--backend", "wlr-protocols", *args],
            env=self.env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=True,
        )

    def close(self) -> None:
        for process in (getattr(self, "prime", None), self.sway):
            if process and process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)


def process_tree(root: int) -> list[int]:
    """`root` and every process below it."""
    children: dict[int, list[int]] = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            stat = (entry / "stat").read_text()
        except OSError:
            continue
        parent = int(stat[stat.rindex(")") + 2 :].split()[1])
        children.setdefault(parent, []).append(int(entry.name))
    found, stack = [], [root]
    while stack:
        pid = stack.pop()
        found.append(pid)
        stack += children.get(pid, [])
    return found


def status(pid: int, field: str) -> int:
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith(field + ":"):
                return int(line.split()[1])
    except (OSError, ValueError):
        pass
    return 0


def switches(pid: int) -> dict[str, int]:
    """Context switches of each of `pid`'s threads so far, by thread: each one is a
    wakeup."""
    counts = {}
    for task in Path(f"/proc/{pid}/task").glob("*"):
        try:
            lines = (task / "status").read_text().splitlines()
        except OSError:
            continue
        counts[task.name] = sum(
            int(line.split()[1]) for line in lines if line.startswith(("voluntary_ctxt", "nonvoluntary_ctxt"))
        )
    return counts


def woken(before: dict[str, int], after: dict[str, int]) -> int:
    """Wakeups between two counts; a thread that ended in between is not counted."""
    return sum(count - before.get(thread, 0) for thread, count in after.items())


class Caret:
    """One Caret with its own bus and in-memory settings, running `command`."""

    def __init__(self, session: Session, caret: Path, schemas: Path, timings: Path, command: list[str]):
        self.timings = timings
        env = session.env | {
            "GSETTINGS_SCHEMA_DIR": str(schemas),
            "GSETTINGS_BACKEND": "memory",
            "CARET_TIMINGS": str(timings),
            "GTK_A11Y": "none",
        }
        self.process = subprocess.Popen(
            ["dbus-run-session", "--", str(caret), "-e", *command],
            env=env,
            stdout=(timings.with_suffix(".log")).open("w"),
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        session.wait(lambda: session.windows() > 0 or self.process.poll() is not None, "the window")
        self.peak_rss = 0

    def caret_pid(self) -> int | None:
        for pid in process_tree(self.process.pid):
            try:
                if Path(f"/proc/{pid}/comm").read_text().strip() == "caret":
                    return pid
            except OSError:
                pass
        return None

    def sample(self) -> None:
        pid = self.caret_pid()
        if pid:
            self.peak_rss = max(self.peak_rss, status(pid, "VmRSS"))

    def wait(self, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        while self.process.poll() is None:
            if time.monotonic() > deadline:
                self.stop()
                raise TimeoutError
            self.sample()
            time.sleep(0.1)

    def stop(self) -> None:
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(5)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait()


def read_timings(path: Path) -> tuple[list[int], list[int]]:
    frames, echoes = [], []
    if path.exists():
        for line in path.read_text().splitlines():
            kind, *values = line.split()
            if kind == "frame":
                frames.append(sum(int(value) for value in values))
            elif kind == "echo":
                echoes.append(int(values[0]))
    return frames, echoes


def percentile(values: list[int], share: float) -> float:
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(share * len(ordered)))] if ordered else float("nan")


def throughput(session, caret, schemas, out, name, path) -> dict:
    result = out / f"{name}.ns"
    result.unlink(missing_ok=True)
    script = f's=$(date +%s%N); cat "{path}"; e=$(date +%s%N); echo $((e - s)) > "{result}"'
    run = Caret(session, caret, schemas, out / f"{name}.timings", ["sh", "-c", script])
    run.wait(300)
    nanoseconds = int(result.read_text())
    frames, _ = read_timings(run.timings)
    return {
        "MB/s": path.stat().st_size / nanoseconds * 1e3,
        "frames": len(frames),
        "frame p50 µs": percentile(frames, 0.5),
        "frame p99 µs": percentile(frames, 0.99),
        "peak RSS MB": run.peak_rss / 1024,
    }


def latency(session, caret, schemas, out) -> dict:
    run = Caret(session, caret, schemas, out / "typing.timings", ["cat"])
    time.sleep(1.0)
    # Typed one at a time, slower than frames come, so each key is answered alone.
    session.wdotool("type", "--delay", "40", "the quick brown fox jumps over the lazy dog " * 3)
    time.sleep(0.5)
    session.wdotool("key", "Return")
    session.wdotool("key", "ctrl+d")
    run.wait(10)
    _, echoes = read_timings(run.timings)
    return {
        "keys": len(echoes),
        "echo p50 µs": percentile(echoes, 0.5),
        "echo p99 µs": percentile(echoes, 0.99),
    }


def idle(session, caret, schemas, out) -> dict:
    run = Caret(session, caret, schemas, out / "idle.timings", ["sleep", "60"])
    # GTK's cursor stops blinking ten seconds after the last key.
    time.sleep(12)
    pid = run.caret_pid()
    if pid is None:
        run.stop()
        sys.exit("caret did not start for the idle run")
    before = switches(pid)
    time.sleep(10)
    after = switches(pid)
    rss = status(pid, "VmRSS")
    run.stop()
    return {"wakeups/s": woken(before, after) / 10, "idle RSS MB": rss / 1024}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--caret", type=Path, default=ROOT / "target/release/caret")
    parser.add_argument("--out", type=Path, default=ROOT / "target/bench")
    parser.add_argument("--runs", type=int, default=3)
    args = parser.parse_args()
    for tool in ("sway", "swaymsg", "wdotool", "dbus-run-session", "glib-compile-schemas"):
        if shutil.which(tool) is None:
            sys.exit(f"{tool} is needed and not installed")
    if not args.caret.exists():
        sys.exit(f"{args.caret} does not exist; build it first")

    args.out.mkdir(parents=True, exist_ok=True)
    files = generate(args.out / "data")
    schemas = args.out / "schemas"
    schemas.mkdir(exist_ok=True)
    shutil.copy(ROOT / f"data/{APP_ID}.gschema.xml", schemas)
    subprocess.run(["glib-compile-schemas", "--strict", str(schemas)], check=True)

    results: dict[str, list[dict]] = {}
    with tempfile.TemporaryDirectory(prefix="caret-bench-") as state:
        session = Session(Path(state))
        try:
            for number in range(args.runs):
                run_out = args.out / f"run-{number}"
                run_out.mkdir(exist_ok=True)
                for name, path in files.items():
                    results.setdefault(name, []).append(throughput(session, args.caret, schemas, run_out, name, path))
                results.setdefault("typing", []).append(latency(session, args.caret, schemas, run_out))
                results.setdefault("idle", []).append(idle(session, args.caret, schemas, run_out))
        finally:
            session.close()

    lines = [f"caret {args.caret}, {RESOLUTION}, median of {args.runs} runs"]
    for name, runs in results.items():
        for key in runs[0]:
            values = [run[key] for run in runs]
            spread = f"  ({min(values):.1f} to {max(values):.1f})" if len(values) > 1 else ""
            lines.append(f"{name:<8} {key:<14} {statistics.median(values):>10.1f}{spread}")
    summary = "\n".join(lines)
    print(summary)
    (args.out / "summary.txt").write_text(summary + "\n")


if __name__ == "__main__":
    main()
