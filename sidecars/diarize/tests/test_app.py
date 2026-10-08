"""Sidecar tests with a stand-in pipeline: no model, token or network. Run with pytest from
sidecars/diarize."""

import io
import sys
from pathlib import Path

import numpy
import soundfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import app  # noqa: E402


class Segment:
    def __init__(self, start, end):
        self.start = start
        self.end = end


class Annotation:
    def __init__(self, tracks):
        self.tracks = tracks

    def itertracks(self, yield_label=False):
        for start, end, label in self.tracks:
            yield Segment(start, end), None, label


def wav(seconds=1.0, rate=16000, channels=1):
    buffer = io.BytesIO()
    samples = numpy.zeros((int(seconds * rate), channels), dtype="float32")
    soundfile.write(buffer, samples, rate, format="WAV", subtype="PCM_16")
    return buffer.getvalue()


def test_labels_follow_first_appearance_and_times_are_milliseconds():
    seen = {}

    def run(audio):
        seen["shape"] = tuple(audio["waveform"].shape)
        seen["rate"] = audio["sample_rate"]
        return Annotation([(2.0, 3.5, "SPEAKER_01"), (0.25, 2.0, "SPEAKER_07")])

    result = app.diarize_bytes(wav(seconds=1.0, channels=2), run=run)
    assert seen == {"shape": (1, 16000), "rate": 16000}, "stereo becomes one channel"
    assert result == {
        "turns": [
            {"speaker": "S1", "start_ms": 250, "end_ms": 2000},
            {"speaker": "S2", "start_ms": 2000, "end_ms": 3500},
        ],
        "speaker_count": 2,
    }


def test_empty_turns_are_dropped():
    turns, count = app.relabel([(1.0, 1.0, "A"), (1.0, 2.0, "B")])
    assert turns == [{"speaker": "S2", "start_ms": 1000, "end_ms": 2000}]
    assert count == 2


def test_device_prefers_the_gpu_and_honours_the_override():
    assert str(app.device("", cuda_available=True)) == "cuda"
    assert str(app.device("", cuda_available=False)) == "cpu"
    assert str(app.device("cpu", cuda_available=True)) == "cpu"
