#!/usr/bin/env python3
"""DER sweep for the NBP diarizer — tune the config with DATA, not by ear.

Workflow:
  1. Drop a few real call clips here:  scripts/diar-eval/clips/<name>.wav
     (convert one: ffmpeg -i ~/nbp-data/<id>/audio_mix.ogg -ar 16000 -ac 1 \
                          scripts/diar-eval/clips/<name>.wav)
  2. Seed editable ground truth:       python3 sweep.py --seed
     → writes <name>.gt.tsv (start  end  speaker) + <name>.hint.txt.
       Open the .gt.tsv and CORRECT the speaker column + boundaries by ear
       (the .hint.txt shows the text per turn to make that fast).
  3. Sweep configs:                    python3 sweep.py
     → runs the sidecar over each (threshold x excludeOverlap x numSpeakers)
       combo on every labeled clip, computes mean DER, prints a sorted table
       and the best config. THEN you change main.swift to the winner.

Requires the built sidecar at src-tauri/binaries/. Models must be cached
(they are if the app has transcribed anything).
"""
import glob
import itertools
import json
import os
import subprocess
import sys

import der as derlib

HERE = os.path.dirname(os.path.abspath(__file__))
CLIPS = os.path.join(HERE, "clips")
SIDECAR = os.path.normpath(
    os.path.join(HERE, "..", "..", "src-tauri", "binaries",
                 "fluidaudio-sidecar-aarch64-apple-darwin")
)

# The grid to sweep. Trim it once you've narrowed the winner.
GRID = {
    "threshold": [0.12, 0.20, 0.28, 0.40, 0.60],
    "exclude_overlap": [True, False],
    "num_speakers": [0, 2],  # 0 = auto (let clustering decide)
}


def run_sidecar(wav, threshold=None, exclude_overlap=None, num_speakers=None):
    cmd = [SIDECAR, wav]
    if threshold is not None:
        cmd += ["--diar-threshold", str(threshold)]
    if exclude_overlap is not None:
        cmd += ["--diar-exclude-overlap", "true" if exclude_overlap else "false"]
    if num_speakers:
        cmd += ["--diar-num-speakers", str(num_speakers)]
    res = subprocess.run(cmd, capture_output=True, text=True)
    if not res.stdout.strip():
        raise RuntimeError(f"sidecar produced no output for {wav}:\n{res.stderr[-500:]}")
    return json.loads(res.stdout)


def labeled_clips():
    return sorted(w for w in glob.glob(os.path.join(CLIPS, "*.wav"))
                  if os.path.exists(w[:-4] + ".gt.tsv"))


def seed():
    wavs = sorted(glob.glob(os.path.join(CLIPS, "*.wav")))
    if not wavs:
        print(f"No clips in {CLIPS}. Add <name>.wav (16 kHz mono) first.")
        return
    for wav in wavs:
        gt = wav[:-4] + ".gt.tsv"
        if os.path.exists(gt):
            print(f"skip {os.path.basename(wav)} (ground truth already exists)")
            continue
        d = run_sidecar(wav)
        with open(gt, "w") as f:
            f.write("# start\tend\tspeaker   <- CORRECT the speaker column + boundaries by ear\n")
            for s in d.get("segments", []):
                f.write(f"{s['startTime']:.2f}\t{s['endTime']:.2f}\t{s['speakerId']}\n")
        with open(wav[:-4] + ".hint.txt", "w") as f:
            for s in d.get("segments", []):
                f.write(f"[{s['startTime']:6.1f}-{s['endTime']:6.1f}] {s['speakerId']}: {s.get('text', '')}\n")
        print(f"seeded {os.path.basename(gt)} — open it and fix the speaker labels")


def sweep():
    clips = labeled_clips()
    if not clips:
        print("No labeled clips. Run `python3 sweep.py --seed`, then correct the .gt.tsv files.")
        return
    combos = list(itertools.product(
        GRID["threshold"], GRID["exclude_overlap"], GRID["num_speakers"]))
    rows = []
    for th, ov, ns in combos:
        ders = []
        for wav in clips:
            gt = derlib.load_gt_tsv(wav[:-4] + ".gt.tsv")
            hyp = run_sidecar(wav, th, ov, ns)
            hyp_segs = [(s["startTime"], s["endTime"], s["speakerId"])
                        for s in hyp.get("segments", [])]
            ders.append(derlib.der(gt, hyp_segs)["der"])
        rows.append((sum(ders) / len(ders), th, ov, ns))
    rows.sort()

    print(f"\n{len(clips)} clip(s) x {len(combos)} configs - DER (lower is better)\n")
    print(f"{'meanDER':>8}  {'thresh':>6}  {'exclOv':>6}  {'numSpk':>6}")
    for mean, th, ov, ns in rows:
        print(f"{mean * 100:6.1f}%  {th:6.2f}  {str(ov):>6}  {(ns or 'auto'):>6}")
    mean, th, ov, ns = rows[0]
    print(f"\nbest: clusteringThreshold={th}  excludeOverlap={ov}  "
          f"numSpeakers={ns or 'auto'}  ->  DER {mean * 100:.1f}%")


if __name__ == "__main__":
    if "--seed" in sys.argv:
        seed()
    else:
        sweep()
