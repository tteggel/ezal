#!/usr/bin/env python3
"""Hardware-free RF regressions: python3 scripts/test_rf.py (requires NumPy)."""
import contextlib
import io
import itertools
import subprocess
import sys
import tempfile
import threading
import unittest
from types import SimpleNamespace
from unittest.mock import patch

import numpy as np

import rftest
import sunscan


class RadioTests(unittest.TestCase):
    def reader(self, data):
        radio = sunscan.Radio.__new__(sunscan.Radio)
        radio.proc = SimpleNamespace(stdout=io.BytesIO(data))
        radio.stderr = io.BytesIO(b"final radio diagnostic\n")
        radio.readings = []
        radio.error = None
        radio.lock = threading.Lock()
        radio.win = np.hanning(sunscan.NFFT).astype(np.float32)
        return radio

    def test_power_clipping_and_timestamp_cover_the_same_block(self):
        nfft, block = sunscan.NFFT, sunscan.BLOCK
        frame = np.random.default_rng(1).integers(-2000, 2001, 2 * nfft, dtype=np.int16)
        frame[-1] = 32767
        data = np.tile(frame, block // nfft)
        # The old leading slice sees only zeros and misses every clipped sample.
        data[:2 * nfft * 8] = 0
        radio = self.reader(data.tobytes())
        with patch.object(sunscan.time, "time", return_value=1000.0):
            radio._reader()
        self.assertEqual(len(radio.readings), 1)
        timestamp, power, clipped = radio.readings[0]
        x = frame.astype(np.float32) / 32768
        spectrum = np.abs(np.fft.fft((x[0::2] + 1j * x[1::2]) * radio.win)) ** 2
        c = nfft // 10
        expected = 10 * np.log10(np.median(np.fft.fftshift(spectrum)[c:-c]) * 120 / 128)
        self.assertAlmostEqual(power, expected, places=5)
        self.assertAlmostEqual(clipped, 120 / (2 * block))
        self.assertEqual(timestamp, 1000.0 - block / sunscan.SR / 2)

    def test_partial_block_is_discarded_and_diagnostic_is_retained(self):
        radio = self.reader(b"\x00" * (4 * sunscan.BLOCK - 1))
        radio._reader()
        self.assertEqual(radio.readings, [])
        self.assertIn("final radio diagnostic", radio.error)

    def test_stderr_larger_than_a_pipe_does_not_block_iq(self):
        popen = subprocess.Popen
        producer = (
            "import sys; "
            "sys.stderr.buffer.write(b'x' * 262144 + b'\\nfinal radio diagnostic\\n'); "
            "sys.stderr.flush(); "
            f"sys.stdout.buffer.write(b'\\x01\\x00' * {2 * sunscan.BLOCK}); "
            "sys.stdout.flush()"
        )
        def start(_cmd, **kwargs):
            return popen([sys.executable, "-c", producer], **kwargs)
        with patch.object(sunscan.subprocess, "Popen", side_effect=start):
            radio = sunscan.Radio(1699.3, 14)
        try:
            radio.proc.wait(timeout=5)
            radio.reader.join(timeout=5)
            self.assertFalse(radio.reader.is_alive())
            self.assertEqual(len(radio.readings), 1)
            self.assertIn("final radio diagnostic", radio.error)
        finally:
            radio.close()


class RunnerTests(unittest.TestCase):
    def test_dish_groups_share_baselines_and_summary_comparisons(self):
        for groups in itertools.permutations(("spur", "lna", "sky")):
            with self.subTest(groups=groups), tempfile.TemporaryDirectory() as directory:
                run = rftest.Runner(SimpleNamespace(dir=directory, gain=14, zoom=1700))
                def record(name, tuned, bias, _text, delay=0):
                    result = {
                        "floor_db": 13 if name == "ground" else 10 if bias else 0,
                        "level_dbfs": -20, "clip_pct": 0,
                        "line": (100, 20), "lines": [(100, 20, 0)],
                    }
                    run.rows.append((name, tuned, bias, result))
                    run.res[name] = result
                    return result
                output = io.StringIO()
                with patch.object(run, "test", side_effect=record), \
                        patch.object(run, "wifi_off_test"), contextlib.redirect_stdout(output):
                    for group in groups:
                        getattr(run, group)()
                    run.summary()
                names = [name for name, *_ in run.rows]
                self.assertEqual(names.count("sky_bias_on"), 1)
                self.assertEqual(names.count("sky_bias_off"), 1)
                self.assertNotIn("sky_1699.3", names)
                summary = output.getvalue()
                self.assertIn("LNA powered vs unpowered (bias on - off): +10.00 dB", summary)
                self.assertIn("ground - cold sky: +3.00 dB", summary)
                for tuning in ("1699.3", "1701", "1702.5"):
                    self.assertIn(f"{tuning} MHz -> +100 Hz 20.0 dB", summary)


if __name__ == "__main__":
    unittest.main()
