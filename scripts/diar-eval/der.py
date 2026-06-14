#!/usr/bin/env python3
"""Diarization Error Rate (DER) for the NBP FluidAudio sidecar.

Time-based DER with optimal speaker-label mapping (brute force over label
permutations — fine for the 1-4 speakers in real calls). No collar; ground
truth is assumed non-overlapping (one speaker per turn), which matches how a
human labels a call by ear.

    DER = (missed + false_alarm + confusion) / total_reference_speech_time

- missed       : reference speech where the hypothesis says silence
- false_alarm  : hypothesis speech where the reference says silence
- confusion    : both speak, but the (optimally mapped) speaker disagrees
"""
import itertools
import json
import sys

FRAME = 0.010  # 10 ms analysis frame


def load_gt_tsv(path):
    """Ground truth: `start<TAB>end<TAB>speaker` per line (Audacity label format).

    Whitespace-separated is also accepted. Lines starting with `#` are comments.
    """
    segs = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 3:
                parts = line.split()
            if len(parts) < 3:
                continue
            segs.append((float(parts[0]), float(parts[1]), parts[2]))
    return segs


def load_hyp_json(path):
    """Hypothesis: sidecar JSON with `segments:[{speakerId,startTime,endTime}]`."""
    d = json.load(open(path))
    return [(s["startTime"], s["endTime"], s["speakerId"]) for s in d.get("segments", [])]


def _frameize(segs, n_frames):
    frames = [None] * n_frames
    for start, end, spk in segs:
        i0 = max(0, int(round(start / FRAME)))
        i1 = min(n_frames, int(round(end / FRAME)))
        for i in range(i0, i1):
            frames[i] = spk
    return frames


def der(gt_segs, hyp_segs):
    """Return a dict with der + components for one clip."""
    dur = max([e for _, e, _ in gt_segs] + [e for _, e, _ in hyp_segs] + [0.0])
    n = int(round(dur / FRAME)) + 1
    g = _frameize(gt_segs, n)
    h = _frameize(hyp_segs, n)

    gt_spk = sorted({s for s in g if s is not None})
    hyp_spk = sorted({s for s in h if s is not None})

    # Try every hyp->gt label mapping; keep the one with the fewest errors.
    targets = gt_spk + [None] * max(0, len(hyp_spk) - len(gt_spk))
    if not hyp_spk:
        mappings = [{}]
    else:
        mappings = [dict(zip(hyp_spk, perm))
                    for perm in set(itertools.permutations(targets, len(hyp_spk)))]

    total_ref = sum(1 for gi in g if gi is not None)
    best = None
    for m in mappings:
        missed = fa = conf = 0
        for gi, hi in zip(g, h):
            if gi is None and hi is None:
                continue
            if hi is None:
                missed += 1
            elif gi is None:
                fa += 1
            elif m.get(hi) != gi:
                conf += 1
        score = missed + fa + conf
        if best is None or score < best[0]:
            best = (score, missed, fa, conf, m)

    _, missed, fa, conf, m = best
    ref = max(total_ref, 1)
    return {
        "der": (missed + fa + conf) / ref,
        "missed": missed / ref,
        "false_alarm": fa / ref,
        "confusion": conf / ref,
        "ref_speech_s": round(total_ref * FRAME, 1),
        "gt_speakers": len(gt_spk),
        "hyp_speakers": len(hyp_spk),
        "mapping": m,
    }


if __name__ == "__main__":
    if len(sys.argv) != 3:
        print("usage: der.py <ground_truth.tsv> <hypothesis.json>", file=sys.stderr)
        sys.exit(2)
    out = der(load_gt_tsv(sys.argv[1]), load_hyp_json(sys.argv[2]))
    print(json.dumps(out, indent=2, ensure_ascii=False))
